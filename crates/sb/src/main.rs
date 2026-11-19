//! The Saltbox CLI: wires the modules into one command tree
//! (capability map, `sb`; spec docs/specs/cli-core.md).

// Output goes through `Write` and `exit::output_error`: `print!`/`eprint!`
// panic when the reader goes away (exit 101) instead of the quiet 141.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::Write;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use sb_cli_core::build_info::{BuildInfo, Channel};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::colour;
use sb_cli_core::config;
use sb_cli_core::exit::{self, RunEnd};
use sb_cli_core::help;
use sb_cli_core::log::{self, RunLog};
use sb_cli_core::root;
use sb_cli_core::run_lock;
use sb_cli_core::theme;
use sb_ops::Checkout;

/// Saltbox CLI. Its derived tree lacks sb-go's hidden `-v` on 14 commands:
/// every parse goes through `verbose::with_flags` (the real parse, the
/// colour pre-pass, the completion tree, the contract test); a
/// `Cli::parse()` or `Cli::command()` alone refuses `-v`.
#[derive(Parser)]
#[command(name = "sb", about = "Saltbox CLI")]
struct Cli {
    /// `--color=auto|always|never` (spec: startup step 5).
    #[command(flatten)]
    color: colour::Color,
    #[command(subcommand)]
    command: Command,
    /// Whether sb-go's `-v/--verbose` was given where sb has no verbose
    /// mode (`verbose`; the maintainer, 2026-11-18).
    #[arg(skip)]
    verbose_given: bool,
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
    // No verbose mode: the run log has the detail (config decision 1);
    // sb-go's `-v` only hidden, with a notice (`verbose`).
    #[command(long_about = "Validate Saltbox configuration files")]
    ValidateConfig {
        /// Print the results as JSON
        // New in sb (config spec, `--json`; decision 9).
        #[arg(long)]
        json: bool,
    },
    /// Change the branch used by Saltbox
    #[command(long_about = "Change the branch used by Saltbox")]
    Branch {
        /// The branch to switch to
        name: String,
        // New (ops decision 42): the switch's local changes question.
        #[command(flatten)]
        discard: sb_ops::discard::DiscardFlag,
    },
    /// Change the branch used by Sandbox
    #[command(long_about = "Change the branch used by Sandbox")]
    BranchSandbox {
        /// The branch to switch to
        name: String,
        #[command(flatten)]
        discard: sb_ops::discard::DiscardFlag,
    },
    /// Update Saltbox & Sandbox
    // sb-go's flags, `-v` only hidden with a notice (ops decision 18,
    // `verbose`), and `--migrations` (decision 19).
    #[command(long_about = "Update Saltbox & Sandbox")]
    Update(sb_ops::update::UpdateArgs),
    /// Edit Saltbox configuration files
    // Alone: the picker (M53). The help is sb-go's without "Editor
    // wrappers should exec the foreground editor so cancellation can stop
    // it" (tui spec, difference 27) and without its GUI-client sentence
    // (difference 34).
    #[command(long_about = EDIT_ABOUT, disable_help_subcommand = true)]
    Edit {
        /// Don't offer to edit the file again when it has issues
        // On `edit` and each subcommand (tui spec, editors): a global
        // flag reaches the subcommands
        // (<https://docs.rs/clap/4.6.7/clap/struct.Arg.html#method.global>).
        #[arg(long, global = true)]
        no_edit_again: bool,
        /// None: the picker.
        #[command(subcommand)]
        command: Option<EditCommand>,
    },
    /// Manage Saltbox configuration facts
    #[command(long_about = fact::ABOUT)]
    Fact,
    /// Display system information
    // No verbose mode (motd decision 9): sb-go's `-v` only hidden, with a notice (`verbose`).
    #[command(long_about = motd::ABOUT)]
    Motd(motd::Args),
    /// Display logs of managed systemd services
    #[command(
        long_about = "Shows Saltbox's systemd services and Docker containers, and the log of the one you choose."
    )]
    Logs,
    /// Update Saltbox CLI
    // Hidden, as sb-go's. No verbose mode (release spec, decision 13):
    // sb-go's `-v` only hidden, with a notice (`verbose`).
    // `--force-update` exists only in builds with self-update off, raw
    // builds included: `parse` adds it then (`force_update_arg`).
    #[command(long_about = "Update Saltbox CLI", hide = true)]
    SelfUpdate {
        /// Automatically accept update without confirmation
        #[arg(short, long)]
        yes: bool,
        /// Switch to this release channel (the new binary keeps it)
        #[arg(long, value_enum)]
        channel: Option<Channel>,
        /// Set from `--force-update` when this build has it.
        #[arg(skip)]
        force_update: bool,
    },
    /// Runs Saltbox diagnostics role
    #[command(long_about = "Runs Saltbox diagnostics role")]
    Diag,
    /// Runs Ansible playbooks with specified tags
    // sb-go's help; `--help` adds how to name tags (ansible spec,
    // difference 17), `-h` and the contract keep sb-go's one line.
    #[command(
        long_about = "Runs Ansible playbooks with specified tags",
        after_long_help = INSTALL_EXAMPLES
    )]
    Install(sb_ansible::InstallArgs),
    /// Install GHA dependencies
    // Hidden, as sb-go's; no flags (ops spec, "`sb gha` (hidden)").
    #[command(long_about = "Install GHA dependencies", hide = true)]
    Gha,
    /// Install Saltbox and its dependencies
    // Hidden, as sb-go's (ops spec, "`sb setup` (hidden)"); its flags are
    // sb-go's, persistent, with update's branch flags (ops decision 29).
    #[command(long_about = "Install Saltbox and its dependencies", hide = true)]
    Setup(sb_ops::setup::SetupArgs),
    /// Update the Ansible virtual environment and saltbox.fact
    // Hidden, as sb-go's. No verbose mode (python-env decision 9):
    // sb-go's `-v` only hidden, with a notice (`verbose`).
    #[command(long_about = UPDATE_VENV_ABOUT, hide = true)]
    UpdateVenv,
    /// List available Saltbox, Sandbox or Saltbox-mod tags
    #[command(long_about = LIST_LONG)]
    List(sb_ansible::ListArgs),
    /// Reinstall the Ansible virtual environment
    // No verbose mode (python-env decision 9): sb-go's `-v` only hidden, with a notice (`verbose`).
    #[command(long_about = "Reinstall the Ansible virtual environment")]
    ReinstallVenv,
    /// Reinstall Saltbox's Python and its Ansible virtual environment using uv
    // No verbose mode (python-env decision 9): sb-go's `-v` only hidden, with a notice (`verbose`).
    // The short help is
    // reworded to fit fang's 120 columns (python-env difference 15); the
    // command's own help keeps sb-go's.
    #[command(
        long_about = "Reinstall the Python version used by Saltbox and related Ansible virtual environment using uv"
    )]
    ReinstallPython,
    /// Reinstall the Rust saltbox.fact file
    // No verbose mode (python-env decision 9): sb-go's `-v` only hidden, with a notice (`verbose`).
    #[command(long_about = "Reinstall the Rust saltbox.fact file")]
    ReinstallFacts,
    /// Install shell completion for sb
    // Hidden, as sb-go's (completion spec, "`sb completion`"); alone, it
    // installs for every installed shell.
    #[command(long_about = COMPLETION_LONG, hide = true, disable_help_subcommand = true)]
    Completion {
        #[command(subcommand)]
        command: Option<CompletionCommand>,
    },
    /// Display announcements from announcement files
    // Shown, not hidden as sb-go's test harness was: it reads
    // announcements now, and migrations stay with `update` (ops decision
    // 27, the maintainer, 2026-11-15). No verbose mode (ops decision 18):
    // sb-go's `-v` only hidden, with a notice (`verbose`). Its two
    // subcommands are hidden (ops decisions 34 and 35): its help, usage line
    // and flags stay as they are (cli-core's help lists only shown
    // subcommands), and no `help` subcommand is added.
    #[command(long_about = ANNOUNCEMENTS_LONG, disable_help_subcommand = true)]
    Announcements {
        #[command(subcommand)]
        command: Option<AnnouncementsCommand>,
    },
    /// Fetches and decrypts files based on username and password
    // Hidden, as sb-go's (ops decision 2). No verbose mode (ops decision
    // 3): sb-go's `-v` only hidden, with a notice (`verbose`); `--user` and `--password-file` are new
    // (decision 8).
    #[command(long_about = RESTORE_LONG, hide = true)]
    Restore(sb_ops::restore::RestoreArgs),
    /// Benchmarks this server's system, disk and network
    // sb's own help: bench is rewritten in Rust (ops difference 15). No
    // flags.
    #[command(long_about = "Benchmarks this server's system, disk and network")]
    Bench,
}

/// `sb update-venv`'s help (sb-go cmd/updateVenv.go, `Long`).
const UPDATE_VENV_ABOUT: &str = "Update the Ansible virtual environment from the current Saltbox checkout and refresh saltbox.fact without updating Git repositories";

/// `sb completion`'s help (sb-go cmd/completion.go, `Long`).
const COMPLETION_LONG: &str =
    "Install system-wide completion for installed Bash, Zsh, and Fish shells.

Bash requires the bash-completion package. Scripts use each shell's shared
loading directory. Generated legacy files are migrated after successful installation.

Restart an existing shell, or source its installed file.
Use \"completion generate <shell>\" to write a script to standard output.";

/// `sb completion`'s subcommands (completion spec).
#[derive(Subcommand, Clone, Copy)]
enum CompletionCommand {
    /// Install bash completion
    #[command(long_about = "Install bash completion")]
    Bash,
    /// Install zsh completion
    #[command(long_about = "Install zsh completion")]
    Zsh,
    /// Install fish completion
    #[command(long_about = "Install fish completion")]
    Fish,
    /// Install completion for installed shells
    #[command(long_about = "Install completion for installed shells")]
    Install {
        /// The one shell to install for
        #[arg(value_name = "bash|zsh|fish")]
        shell: Option<sb_completion::Shell>,
        /// Allow an unavailable shell during automatic completion refresh
        // Hidden, as sb-go's: released sb-go 0.1.1's self-update passes it
        // to the new binary (spec, decision 8).
        #[arg(long, hide = true)]
        allow_unavailable: bool,
    },
    /// Write a completion script to standard output
    #[command(long_about = "Write a completion script to standard output")]
    Generate {
        /// The shell to write it for
        // sb-go's hint, `<bash|zsh|fish>`; another shell is a usage error.
        #[arg(value_name = "bash|zsh|fish")]
        shell: sb_completion::Shell,
    },
}

/// `sb announcements`' hidden subcommands, for developers (ops spec,
/// "`sb announcements preview`" and "`sb announcements check`").
#[derive(Subcommand, Clone)]
enum AnnouncementsCommand {
    /// Show what the next sb update would show of the announcements
    #[command(long_about = PREVIEW_LONG, hide = true)]
    Preview {
        /// The git ref the announcements are compared with
        #[arg(long, value_name = "ref", default_value = "HEAD")]
        since: String,
        /// Preview one repository only
        #[arg(long, value_enum, value_name = "saltbox|sandbox")]
        repo: Option<RepoArg>,
        /// Run the migrations when approved, not a dry run
        #[arg(long)]
        run_migrations: bool,
    },
    /// Check announcement files for problems
    #[command(long_about = CHECK_LONG, hide = true)]
    Check {
        /// The file to check (default: both repositories' own files)
        file: Option<std::path::PathBuf>,
        /// The repository the file's migration tags belong to
        #[arg(long, value_enum, value_name = "saltbox|sandbox")]
        repo: Option<RepoArg>,
    },
}

/// `--repo`'s values.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum RepoArg {
    Saltbox,
    Sandbox,
}

impl RepoArg {
    fn checkout(self) -> sb_ops::Checkout {
        match self {
            Self::Saltbox => sb_ops::Checkout::Saltbox,
            Self::Sandbox => sb_ops::Checkout::Sandbox,
        }
    }
}

/// `sb announcements preview`'s long help.
const PREVIEW_LONG: &str = "Show what the next sb update would show of the announcements

Compares each repository's announcements.yml at a git ref (--since,
HEAD by default, so uncommitted edits count as new) with its working
tree, as sb update compares them around its pull: the viewer on a
terminal, the new ones printed when piped, then their migrations. The
migrations are a dry run unless --run-migrations.";

/// `sb announcements check`'s long help.
const CHECK_LONG: &str = "Check announcement files for problems

Checks a file, or both repositories' own files: each entry's fields,
date, paths, Markdown and migration, and its migration tag against the
repository's cached tags. Exits 1 when there are problems.";

/// `sb announcements`' long help: what it reads now that it is shown
/// (ops spec, "`sb announcements`").
const ANNOUNCEMENTS_LONG: &str = "Display Saltbox's and Sandbox's announcements

Reads each repository's announcements.yml and shows every announcement
that applies, each labelled with its repository: on a terminal in a
viewer, printed when piped. Migrations run in sb update.";

/// `sb restore`'s long help: sb-go's (cmd/restore.go), the flags' answers
/// in place of its form.
const RESTORE_LONG: &str = "Fetches encrypted files from a remote URL, decrypts them, and places them in the Saltbox directory.

The restore URL defaults to \"crs.saltbox.dev\". On a terminal the user name and the password
are asked for in a form unless --user and --password-file give them; without one, both flags
are needed. --user shows in the process list and in sudo's log: leave it out to be asked
instead.";

/// `sb install --help`'s examples, in the style of [`LIST_LONG`]'s (ansible
/// spec, difference 17): both tag separators, the Sandbox and mod
/// prefixes, and `sb list`.
const INSTALL_EXAMPLES: &str = "Examples:
  sb install plex sonarr       # Two Saltbox tags
  sb install plex,sonarr       # The same, with commas
  sb install \"plex, sonarr\"    # Commas and spaces, quoted
  sb install sandbox-jellyfin  # Sandbox: sandbox-<tag>
  sb install mod-myrole        # Saltbox mod: mod-<tag>
  sb list                      # Every tag there is";

/// `sb list`'s long help, sb-go's (cmd/list.go).
const LIST_LONG: &str = "List available Saltbox, Sandbox or Saltbox-mod tags

Without arguments, displays all available tags.
With a query argument, performs fuzzy search across all tags.

Examples:
  sb list                # List all tags
  sb list plex           # Search for tags matching \"plex\"
  sb list arr            # Search for tags matching \"arr\"";

/// The host check's verdict for this run: an unsupported *Ubuntu
/// release* lets `version` and the standalone `self-update` through
/// (`allowed`), with a warning saying so, since self-update is how such a
/// host reaches a binary that supports it (maintainer, 2026-11-18;
/// cli-core spec, startup step 1). Anything else stands.
fn allow_unsupported_release(
    support: Result<(), sb_host::Unsupported>,
    allowed: bool,
    warnings: &mut Vec<String>,
) -> Result<(), sb_host::Unsupported> {
    match support {
        Err(sb_host::Unsupported::Release { found, supported }) if allowed => {
            warnings.push(format!(
                "Ubuntu {found} isn't a supported release (supported versions: {supported}); \
                 sb self-update may bring a version that supports it"
            ));
            Ok(())
        }
        other => other,
    }
}

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
// Each also takes its file's name, and the inventory its own
// (`localhost`), as hidden aliases: the names users type from the files
// (tui spec, editors; clap's `Command::alias`,
// <https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.alias>).
enum EditCommand {
    /// Accounts
    #[command(alias = "accounts.yml")]
    Accounts,
    /// Advanced Settings
    #[command(name = "adv_settings", alias = "adv_settings.yml")]
    AdvSettings,
    /// Backup Settings
    #[command(name = "backup_config", alias = "backup_config.yml")]
    BackupConfig,
    /// Hetzner VLAN Settings
    #[command(name = "hetzner_vlan", alias = "hetzner_vlan.yml")]
    HetznerVlan,
    /// Settings
    #[command(alias = "settings.yml")]
    Settings,
    /// Inventory Settings
    #[command(aliases = ["localhost", "localhost.yml"])]
    Inventory,
    /// Motd Settings
    #[command(alias = "motd.yml")]
    Motd,
    /// Providers
    #[command(alias = "providers.yml")]
    Providers,
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
            Self::Motd => edit::File::Motd,
            Self::Providers => edit::File::Providers,
        }
    }
}

/// `sb docker`'s subcommands (docker spec). sb-go's `logs` is gone:
/// `sb logs` shows the containers too (the maintainer, 2026-11-18; tui
/// decision 1), and `hinted` names it.
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
    /// Start Docker containers managed by Saltbox
    // No verbose mode (docker decision 6): sb-go's `-v` only hidden, with a notice (`verbose`).
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
                DockerCommand::Ps { .. } => "docker ps",
                DockerCommand::Start => "docker start",
                DockerCommand::Stop { .. } => "docker stop",
                DockerCommand::Restart { .. } => "docker restart",
            },
            Command::ValidateConfig { .. } => "validate-config",
            Command::Branch { .. } => "branch",
            Command::BranchSandbox { .. } => "branch-sandbox",
            Command::Update(_) => "update",
            Command::Edit { command, .. } => match command {
                None => "edit",
                Some(EditCommand::Accounts) => "edit accounts",
                Some(EditCommand::AdvSettings) => "edit adv_settings",
                Some(EditCommand::BackupConfig) => "edit backup_config",
                Some(EditCommand::HetznerVlan) => "edit hetzner_vlan",
                Some(EditCommand::Settings) => "edit settings",
                Some(EditCommand::Inventory) => "edit inventory",
                Some(EditCommand::Motd) => "edit motd",
                Some(EditCommand::Providers) => "edit providers",
            },
            Command::Fact => "fact",
            Command::Motd(_) => "motd",
            Command::SelfUpdate { .. } => "self-update",
            Command::Logs => "logs",
            Command::Diag => "diag",
            Command::Install(_) => "install",
            Command::Gha => "gha",
            Command::Setup(_) => "setup",
            Command::UpdateVenv => "update-venv",
            Command::List(_) => "list",
            Command::ReinstallVenv => "reinstall-venv",
            Command::ReinstallPython => "reinstall-python",
            Command::ReinstallFacts => "reinstall-facts",
            Command::Announcements { command } => match command {
                None => "announcements",
                Some(AnnouncementsCommand::Preview { .. }) => "announcements preview",
                Some(AnnouncementsCommand::Check { .. }) => "announcements check",
            },
            Command::Restore(_) => "restore",
            Command::Bench => "bench",
            Command::Completion { command } => match command {
                None => "completion",
                Some(CompletionCommand::Bash) => "completion bash",
                Some(CompletionCommand::Zsh) => "completion zsh",
                Some(CompletionCommand::Fish) => "completion fish",
                Some(CompletionCommand::Install { .. }) => "completion install",
                Some(CompletionCommand::Generate { .. }) => "completion generate",
            },
        }
    }

    /// Whether the command takes the run lock (cli-core spec, "Run
    /// lock"; the maintainer, 2026-11-18): the commands that change the
    /// host. Every command decides here, with no wildcard arm, as for
    /// root.
    fn takes_run_lock(&self) -> bool {
        match self {
            Command::Install(_)
            | Command::Update(_)
            | Command::Setup(_)
            | Command::Gha
            | Command::Branch { .. }
            | Command::BranchSandbox { .. }
            | Command::SelfUpdate { .. }
            | Command::UpdateVenv
            | Command::ReinstallVenv
            | Command::ReinstallPython
            | Command::ReinstallFacts
            | Command::Restore(_)
            // A preview that runs its migrations changes the host as
            // update's migrations do (ops decision 34).
            | Command::Announcements {
                command: Some(AnnouncementsCommand::Preview { run_migrations: true, .. }),
            } => true,
            // Read-only or interactive; motd never waits (pam_motd runs
            // it at login); diag's role only reports; docker start, stop
            // and restart go through the controller's own jobs; list's
            // tag cache and completion's files are written atomically.
            Command::Version { .. }
            | Command::Docker { .. }
            | Command::ValidateConfig { .. }
            | Command::Edit { .. }
            | Command::Fact
            | Command::Motd(_)
            | Command::Logs
            | Command::Diag
            | Command::List(_)
            | Command::Announcements { .. }
            | Command::Bench
            | Command::Completion { .. } => false,
        }
    }

    /// Whether the command runs on an unsupported Ubuntu release, with a
    /// warning: `version` and the standalone `self-update` only (cli-core
    /// spec, startup step 1). `update` keeps refusing: it changes the host.
    fn runs_on_unsupported_release(&self) -> bool {
        matches!(self, Self::Version { .. } | Self::SelfUpdate { .. })
    }

    /// Whether the command needs root (spec: startup step 3). Every command
    /// decides here, with no wildcard arm, so a new one can't skip it.
    fn needs_root(&self) -> bool {
        match self {
            // `completion generate` prints a stub and reads nothing
            // (completion spec, decision 4).
            Command::Version { .. }
            | Command::Completion {
                command: Some(CompletionCommand::Generate { .. }),
            } => false,
            // The Docker socket, and sb's rule that only version and
            // completion run unprivileged (cli-core decision 3); Saltbox's
            // config is root's to read (config spec); the Saltbox and
            // Sandbox checkouts and their ownership are root's to change;
            // so are the files `edit` opens and the facts `fact` edits (tui
            // spec, editors, assumption 2); the journal files are root's to
            // read (tui assumption 2); motd as every
            // command (motd decision 20: pam_motd already runs it as root);
            // sb's own file, which `self-update` replaces (release spec,
            // assumption 5); playbooks run with `--become` from root's
            // Saltbox checkout (ansible spec, success criterion 9); list
            // writes that checkout's tag cache; the venv, its wrappers and
            // apt, which `update-venv`, `reinstall-venv` and
            // `reinstall-python` change, and the checkout's fact, which
            // `reinstall-facts` replaces (python-env assumption 5);
            // `announcements`, `gha`, `setup` and `bench`, as every ops
            // command (ops assumption 2; gha's and setup's apt, /srv and
            // the configs).
            Command::Docker { .. }
            | Command::ValidateConfig { .. }
            | Command::Branch { .. }
            | Command::BranchSandbox { .. }
            | Command::Update(_)
            | Command::Edit { .. }
            | Command::Fact
            | Command::Motd(_)
            | Command::SelfUpdate { .. }
            | Command::Logs
            | Command::Diag
            | Command::Install(_)
            | Command::Gha
            | Command::Setup(_)
            | Command::UpdateVenv
            | Command::List(_)
            | Command::ReinstallVenv
            | Command::ReinstallPython
            | Command::ReinstallFacts
            | Command::Announcements { .. }
            | Command::Restore(_)
            | Command::Bench
            // The installing forms write the shells' system directories
            // (completion spec, assumption 4).
            | Command::Completion {
                command:
                    None
                    | Some(
                        CompletionCommand::Bash
                        | CompletionCommand::Zsh
                        | CompletionCommand::Fish
                        | CompletionCommand::Install { .. },
                    ),
            } => true,
        }
    }
}

/// The run lock for `command`, when it takes it (cli-core spec, "Run
/// lock"): waiting with one line on stderr while another sb holds it,
/// then while Saltbox's backup holds its own lock in the checkout; the run
/// log gets how long it waited, or the ancestor whose lock this run is
/// under (which waited for the backup already).
///
/// # Errors
///
/// cli-core's [`run_lock::LockError`]: the lock's directory or file isn't
/// safe or can't be opened, or Ctrl-C or a signal came while waiting.
fn take_run_lock(
    command: &Command,
    shutdown: &Shutdown,
) -> anyhow::Result<Option<run_lock::Taken>> {
    if !command.takes_run_lock() {
        return Ok(None);
    }
    let name = format!("sb {}", command.name());
    let taken = run_lock::take(&run_lock::dir(), &name, &shutdown.token(), |holder| {
        let line = exit::escaped(&run_lock::waiting_line(holder));
        let _ = writeln!(anstream::stderr(), "{line}");
    })?;
    match &taken {
        run_lock::Taken::Held {
            waited, waited_for, ..
        } => tracing::info!(
            waited_ms = waited.as_millis(),
            holder_pid = waited_for.as_ref().map(|holder| holder.pid),
            holder_command = waited_for.as_ref().map(|holder| holder.command.as_str()),
            "run lock taken"
        ),
        run_lock::Taken::Nested(holder) => {
            tracing::info!(
                holder_pid = holder.pid,
                holder_command = holder.command.as_str(),
                "run lock held by an ancestor sb; running under it"
            );
            return Ok(Some(taken));
        }
    }
    let checkout = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let waited = run_lock::wait_for_backup(&checkout, &shutdown.token(), |since| {
        let line = exit::escaped(&run_lock::backup_waiting_line(&shown_time(since)));
        let _ = writeln!(anstream::stderr(), "{line}");
    })?;
    if let Some(waited) = waited {
        tracing::info!(
            waited_ms = waited.waited.as_millis(),
            since = shown_time(waited.since),
            backup_pid = waited.pid,
            "waited for a Saltbox backup"
        );
    }
    Ok(Some(taken))
}

/// `at` as sb shows a time: the local zone's (`/etc/localtime`, as
/// sb-tui's), seconds and the zone's name, as bench's timestamp; a time
/// jiff can't hold is its debug form.
fn shown_time(at: std::time::SystemTime) -> String {
    jiff::Timestamp::try_from(at).map_or_else(
        |_| format!("{at:?}"),
        |at| {
            at.to_zoned(sb_tui::line::local_zone())
                .strftime("%Y-%m-%d %H:%M:%S %Z")
                .to_string()
        },
    )
}

fn main() -> RunEnd {
    // A completion request first, before anything is printed or read
    // (clap_complete's rule; completion spec, "Tab completion"): it prints
    // only candidates, as the calling user, and is never logged (cli-core
    // spec, startup). Any other run returns at once.
    sb_completion::complete(completion_tree);
    // Colour and theme first (from the arguments, the environment and the
    // theme file), so every message below follows `--color` and the
    // profile. The colour
    // choice must be anstream's global one before the real parse, which
    // prints help (spec: startup steps 5 and 6).
    colour::early_choice(verbose::with_flags(Cli::command()), std::env::args_os()).write_global();
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
    let parsed = parse(profile);
    let support = allow_unsupported_release(
        sb_host::check_support(),
        parsed
            .as_ref()
            .is_ok_and(|cli| cli.command.runs_on_unsupported_release()),
        &mut warnings,
    );
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
    // sb-go's `-v`, accepted where sb has no verbose mode: a notice before
    // the steps, the run otherwise as without it (the maintainer,
    // 2026-11-18). Only the run that runs the command gets here: a run
    // handing over to sudo was replaced above.
    if cli.verbose_given {
        tracing::info!("-v given: accepted and ignored, sb has no verbose mode");
        let pointer = log.as_ref().map(RunLog::pointer);
        notice(verbose::notice(pointer.as_deref()));
    }
    let shutdown = Shutdown::new();
    let result = shutdown
        .listen_for_signals()
        .map_err(anyhow::Error::from)
        .and_then(|()| {
            // Held until the command is done (cli-core spec, "Run lock").
            let _lock = take_run_lock(&cli.command, &shutdown)?;
            run(&cli.command, &shutdown, log.as_ref())
        });
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

/// sb's tree for a completion request: the tag completer on `install`'s
/// tags (completion spec, "Tags for `sb install`"). Added here, not in
/// `InstallArgs`: the completer reads ansible's cache, so sb-ansible can't
/// depend on it.
fn completion_tree() -> clap::Command {
    use sb_completion::tags;
    verbose::with_flags(Cli::command()).mut_subcommand(tags::INSTALL, |install| {
        install.mut_arg(tags::TAGS, |arg| arg.add(tags::completer(Cli::command)))
    })
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
    let cmd = verbose::with_flags(cmd);
    let cmd = help::apply(cmd, theme::current(), colour, help::width());
    cmd.try_get_matches().and_then(|matches| {
        let mut cli = Cli::from_arg_matches(&matches)?;
        cli.verbose_given = verbose::given(&matches);
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
    let args: Vec<String> = std::env::args_os()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let err = hinted(err, &args);
    let block = help::parse_error_block(err, theme::current(), help::error_width());
    let _ = write!(anstream::stderr(), "{block}");
    std::process::exit(code)
}

/// `err` with a hint for three mistakes clap can't explain, as one of its
/// own tips (`ContextKind::Suggested`, which clap renders as `tip:` and
/// the error block as `hint:`;
/// <https://docs.rs/clap/4.6.7/clap/error/enum.ContextKind.html#variant.Suggested>),
/// after any tip clap gave: `sb install` with no tags names `sb list`, a
/// name given to `sb docker restart|stop|start`, which act on every
/// Saltbox container, names Docker's own command for one, and sb-go's
/// `sb docker logs`, which sb doesn't have, names `sb logs` (tui decision
/// 1; the word typed is clap's `ContextKind::InvalidSubcommand`,
/// <https://docs.rs/clap/4.6.7/clap/error/enum.ContextKind.html#variant.InvalidSubcommand>).
/// A typo's suggested subcommands (`ContextKind::SuggestedSubcommand`) are
/// named once each, by the command an alias opens: clap suggests hidden
/// aliases too (`Command::all_subcommand_names`), so `sb edit acounts`
/// would offer `accounts.yml` beside `accounts`. `args` is the command
/// line.
fn hinted(mut err: clap::Error, args: &[String]) -> clap::Error {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    let path = command_path(args);
    if let Some(ContextValue::Strings(names)) = err.get(ContextKind::SuggestedSubcommand) {
        let names = canonical(&path, names);
        err.insert(
            ContextKind::SuggestedSubcommand,
            ContextValue::Strings(names),
        );
    }
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    let typed = match err
        .get(ContextKind::InvalidArg)
        .or_else(|| err.get(ContextKind::InvalidSubcommand))
    {
        Some(ContextValue::String(value)) => Some(value.clone()),
        _ => None,
    };
    let hint = match (path.as_slice(), err.kind(), typed) {
        (["install"], ErrorKind::MissingRequiredArgument, _) => {
            Some("to see the tags, use 'sb list'".to_string())
        }
        (
            ["docker", action @ ("restart" | "stop" | "start")],
            ErrorKind::UnknownArgument,
            Some(name),
        ) if !name.starts_with('-') => Some(format!(
            "sb docker {action} acts on every Saltbox container; for one container, use \
                 'docker {action} {name}'"
        )),
        (["docker"], ErrorKind::InvalidSubcommand, Some(name)) if name == "logs" => Some(
            "for container logs, use 'sb logs': it shows Saltbox's services and Docker \
             containers"
                .to_string(),
        ),
        _ => None,
    };
    if let Some(hint) = hint {
        let mut tips = match err.get(ContextKind::Suggested) {
            Some(ContextValue::StyledStrs(tips)) => tips.clone(),
            _ => Vec::new(),
        };
        tips.push(hint.into());
        err.insert(ContextKind::Suggested, ContextValue::StyledStrs(tips));
    }
    err
}

/// The subcommands `args` (sb's command line) names, outermost first:
/// clap's own partial parse (`Command::ignore_errors`, as cli-core's
/// colour pre-pass reads the line;
/// <https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.ignore_errors>),
/// which records each subcommand by its name, even when the error is
/// inside it. Empty when even that parse fails.
fn command_path(args: &[String]) -> Vec<String> {
    let Ok(matches) = Cli::command()
        .ignore_errors(true)
        .try_get_matches_from(args)
    else {
        return Vec::new();
    };
    let mut path = Vec::new();
    let mut at = &matches;
    while let Some((name, sub)) = at.subcommand() {
        path.push(name.to_string());
        at = sub;
    }
    path
}

/// `names`, subcommands of the command at `path` as clap suggests them
/// (by ascending similarity), each read as its command's own name, once,
/// where its best match stood.
fn canonical(path: &[String], names: &[String]) -> Vec<String> {
    let mut cmd = Cli::command();
    for name in path {
        let Some(sub) = cmd.find_subcommand(name).cloned() else {
            return names.to_vec();
        };
        cmd = sub;
    }
    let read: Vec<String> = names
        .iter()
        .map(|name| {
            cmd.find_subcommand(name)
                .map_or_else(|| name.clone(), |sub| sub.get_name().to_string())
        })
        .collect();
    // The last of each, clap's best match last.
    read.iter()
        .enumerate()
        .filter(|(i, name)| !read[i + 1..].contains(name))
        .map(|(_, name)| name.clone())
        .collect()
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

/// A notice line on stderr before any step, as term-ui marks a warning
/// notice under a step (an orange `!`, the theme's warn colour), through
/// anstream so `--color` and `NO_COLOR` are honoured; escaped, as
/// [`warn`].
fn notice(message: impl std::fmt::Display) {
    let style = theme::current().style(theme::Role::Warn);
    let message = exit::escaped(&message.to_string());
    let _ = writeln!(anstream::stderr(), "{style}!{style:#} {message}");
}

fn run(command: &Command, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    match *command {
        Command::ValidateConfig { json } => validate_config(json, shutdown, log)?,
        Command::Branch { ref name, discard } => branch::branch(
            Checkout::Saltbox,
            name,
            discard.discard_local,
            shutdown,
            log,
        )?,
        Command::BranchSandbox { ref name, discard } => {
            branch::branch(
                Checkout::Sandbox,
                name,
                discard.discard_local,
                shutdown,
                log,
            )?;
        }
        Command::Edit {
            command,
            no_edit_again,
        } => match command {
            Some(command) => edit::edit(command.file(), no_edit_again, shutdown, log)?,
            None => edit::pick(no_edit_again, shutdown, log)?,
        },
        Command::Fact => fact::fact(shutdown)?,
        Command::Motd(ref args) => motd::motd(args, shutdown, log)?,
        Command::Gha => gha::gha(shutdown, log)?,
        Command::Setup(ref args) => setup::setup(args, shutdown, log)?,
        Command::Update(ref args) => update::update(args, shutdown, log)?,
        Command::UpdateVenv => python_env::update_venv(shutdown, log)?,
        Command::ReinstallVenv => python_env::reinstall_venv(shutdown, log)?,
        Command::ReinstallPython => python_env::reinstall_python(shutdown, log)?,
        Command::ReinstallFacts => python_env::reinstall_facts(shutdown, log)?,
        Command::Announcements { ref command } => match command {
            None => announcements::announcements(shutdown, log)?,
            Some(AnnouncementsCommand::Preview {
                since,
                repo,
                run_migrations,
            }) => announcements::preview(
                since,
                repo.map(RepoArg::checkout),
                *run_migrations,
                shutdown,
                log,
            )?,
            Some(AnnouncementsCommand::Check { file, repo }) => {
                announcements::check(file.as_deref(), repo.map(RepoArg::checkout), shutdown)?;
            }
        },
        Command::Restore(ref args) => restore::restore(args, shutdown)?,
        Command::Bench => bench::bench(shutdown, log)?,
        Command::SelfUpdate {
            yes,
            channel,
            force_update,
        } => {
            self_update::self_update(yes, force_update, channel, shutdown, log)?;
        }
        Command::Logs => logs::logs(shutdown)?,
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
        Command::Completion { command } => completion::completion(command, shutdown)?,
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
    let address = docker::address()?;
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
/// exit. The HTTP client is built at the first credential check's
/// request; one that can't be built (TLS setup) fails each check that
/// needs it, in its own block. An old-format
/// motd.yml is migrated first, its original copied to `SB_BACKUPS_DIR`,
/// or sb-config's default, `/opt/saltbox/backups` (motd decision 4; a
/// temporary folder in sb's test build).
fn validate_config(json: bool, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let settings = validate_settings()?;
    // The runtime's error first (`?`), then the run's, as the result.
    block_on(async {
        let options = sb_term_ui::Options::default();
        // No log: the pointer comes once, after the tree, then the edits
        // that fix the files with issues (`edit::print_closing`), on
        // stderr with the steps; `--json`'s stdout holds the document
        // alone.
        let mut runner = sb_term_ui::Runner::start("sb validate-config", shutdown, None, options);
        let (result, results) =
            sb_config::run::run_recording(&mut runner, &settings, &backups_dir()).await;
        runner.finish();
        if result.is_err() {
            let fix: Vec<edit::File> = results
                .with_issues()
                .filter_map(edit::File::of_path)
                .collect();
            edit::print_closing(log, &fix, shutdown);
        }
        if let Some(document) = json_document(json, shutdown.token().is_cancelled(), &results) {
            let mut out = std::io::stdout().lock();
            out.write_all(document.as_bytes())
                .and_then(|()| out.flush())
                .map_err(exit::output_error)?;
        }
        result.map_err(anyhow::Error::from)
    })?
}

/// validate-config's `--json` document, for stdout after the steps drew
/// on stderr; none without `--json`, and none for a run that stopped
/// (config spec, `--json`).
fn json_document(json: bool, stopped: bool, results: &sb_config::run::Results) -> Option<String> {
    (json && !stopped).then(|| sb_config::run::render_json(results))
}

/// validate-config's settings, `sb update`'s too: the Saltbox root, the
/// zoneinfo places, the HTTP client (built at its first request) and
/// the services' URLs.
///
/// # Errors
///
/// The proxy settings can't be read (`Http::new`; with
/// `Proxy::System` it reads no URL, so in practice none).
fn validate_settings() -> anyhow::Result<sb_config::run::Settings> {
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
    Ok(settings)
}

/// Where motd.yml's migration backs the original up (`SB_BACKUPS_DIR`,
/// else config's default): validate-config's and `sb update`'s.
fn backups_dir() -> std::path::PathBuf {
    std::env::var_os("SB_BACKUPS_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(
            sb_config::migrate::default_backups,
            std::path::PathBuf::from,
        )
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
mod bench;
mod branch;
mod completion;
mod docker;
mod edit;
mod fact;
mod gha;
mod logs;
mod motd;
mod python_env;
mod restore;
mod screen;
mod self_update;
mod setup;
mod update;
mod verbose;

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
        let raw = BuildInfo::from_env("0.1.0", None, None, Channel::None);
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
        let release = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, Channel::Release);
        let beta = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, Channel::Beta);
        let disabled = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, Channel::None);
        let raw = BuildInfo::from_env("0.1.0", None, None, Channel::Release);
        assert!(!has_it(&release));
        assert!(!has_it(&beta));
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
            channel: None,
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
            &BuildInfo::from_env("0.1.0", None, None, Channel::None),
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
    fn validate_config_json_is_written_only_for_a_whole_run() {
        let results = sb_config::run::Results::default();
        let document = json_document(true, false, &results).expect("a document");
        assert!(document.starts_with(r#"{"schema":1,"#), "{document}");
        assert_eq!(json_document(true, true, &results), None);
        assert_eq!(json_document(false, false, &results), None);
    }

    #[test]
    fn validate_config_needs_root() {
        let command = Command::ValidateConfig { json: false };
        assert!(command.needs_root());
        assert_eq!(command.name(), "validate-config");
    }

    /// Both branch commands change a root-owned checkout and its
    /// ownership, and log under sb-go's names.
    #[test]
    fn branch_commands_need_root_and_log_their_names() {
        for (args, name) in [
            (["sb", "branch", "develop"], "branch"),
            (["sb", "branch-sandbox", "develop"], "branch-sandbox"),
        ] {
            let command = Cli::try_parse_from(args).expect("parses").command;
            assert!(command.needs_root(), "{name}");
            assert_eq!(command.name(), name);
        }
    }

    /// `sb install` with no tags, `sb docker restart|stop|start` given a
    /// name, and sb-go's `sb docker logs` get a hint naming the next
    /// command; other errors keep clap's own lines.
    #[test]
    fn context_hints_name_the_next_command() {
        let block = |args: &[&str]| {
            let err = Cli::command()
                .try_get_matches_from(args)
                .expect_err("a parse error");
            let args: Vec<String> = args.iter().map(ToString::to_string).collect();
            help::parse_error_block(hinted(err, &args), &theme::Theme::plain(), help::MAX_WIDTH)
        };
        let install = block(&["sb", "install"]);
        assert!(
            install.contains("\n  hint: to see the tags, use 'sb list'\n"),
            "{install}"
        );
        let with_colour = block(&["sb", "--color=never", "install"]);
        assert!(with_colour.contains("use 'sb list'"), "{with_colour}");
        for action in ["restart", "stop", "start"] {
            let docker = block(&["sb", "docker", action, "plex"]);
            assert!(
                docker.contains(&format!(
                    "\n  hint: sb docker {action} acts on every Saltbox container; for one \
                     container, use 'docker {action} plex'\n"
                )),
                "{docker}"
            );
        }
        let flag = block(&["sb", "docker", "restart", "--bogus"]);
        assert!(!flag.contains("Saltbox container"), "{flag}");
        // `sb docker logs` is gone (tui decision 1): `sb logs` shows both.
        let sb_logs = "\n  hint: for container logs, use 'sb logs': it shows Saltbox's services \
                       and Docker containers\n";
        for args in [
            &["sb", "docker", "logs"][..],
            &["sb", "--color=never", "docker", "logs", "--help"],
        ] {
            let gone = block(args);
            assert!(gone.contains(sb_logs), "{args:?}: {gone}");
        }
        let unknown = block(&["sb", "docker", "nope"]);
        assert!(!unknown.contains("sb logs"), "{unknown}");
        let elsewhere = block(&["sb", "logs", "logs"]);
        assert!(!elsewhere.contains("hint:"), "{elsewhere}");
        let other = block(&["sb", "version", "plex"]);
        assert!(!other.contains("hint:"), "{other}");
        let docker = block(&["sb", "docker", "ps", "plex"]);
        assert!(!docker.contains("Saltbox container"), "{docker}");
        // After flags and `--color`'s own value, the same.
        let late = block(&["sb", "--color", "never", "docker", "stop", "plex"]);
        assert!(
            late.contains("for one container, use 'docker stop plex'"),
            "{late}"
        );
    }

    /// A typo's suggestions name each file once, by its subcommand: the
    /// hidden file-name aliases (clap suggests every alias) are read as
    /// the command they open, the best guess still first.
    #[test]
    fn suggestions_name_each_command_once() {
        let block = |args: &[&str]| {
            let err = Cli::command()
                .try_get_matches_from(args)
                .expect_err("a parse error");
            let args: Vec<String> = args.iter().map(ToString::to_string).collect();
            help::parse_error_block(hinted(err, &args), &theme::Theme::plain(), help::MAX_WIDTH)
        };
        let accounts = block(&["sb", "edit", "acounts"]);
        assert!(
            accounts.contains("hint: did you mean 'accounts'?\n"),
            "{accounts}"
        );
        let settings = block(&["sb", "edit", "setings"]);
        assert!(
            settings.contains("hint: did you mean 'settings' or 'adv_settings'?\n"),
            "{settings}"
        );
        let inventory = block(&["sb", "edit", "localhst"]);
        assert!(
            inventory.contains("did you mean 'inventory'?"),
            "{inventory}"
        );
        assert!(!inventory.contains("'localhost'"), "{inventory}");
        // Elsewhere clap's own names stay.
        let version = block(&["sb", "verison"]);
        assert!(version.contains("did you mean 'version'?"), "{version}");
    }

    /// `sb edit` takes each file's own name, and the inventory's
    /// `localhost`, as the subcommand that opens it.
    #[test]
    fn edit_takes_the_file_names_too() {
        for (name, file) in [
            ("accounts.yml", edit::File::Accounts),
            ("settings.yml", edit::File::Settings),
            ("adv_settings.yml", edit::File::AdvSettings),
            ("backup_config.yml", edit::File::BackupConfig),
            ("hetzner_vlan.yml", edit::File::HetznerVlan),
            ("localhost", edit::File::Inventory),
            ("localhost.yml", edit::File::Inventory),
            ("motd.yml", edit::File::Motd),
            ("providers.yml", edit::File::Providers),
        ] {
            let parsed = Cli::try_parse_from(["sb", "edit", name])
                .unwrap_or_else(|err| panic!("{name}: {err}"))
                .command;
            let Command::Edit {
                command: Some(command),
                ..
            } = parsed
            else {
                panic!("{name}: not an edit subcommand");
            };
            assert_eq!(command.file(), file, "{name}");
        }
    }

    /// Each file's `sb edit` name, as a `Fix with:` line gives it, parses
    /// to the subcommand that opens that file.
    #[test]
    fn each_files_edit_name_opens_it() {
        for file in edit::File::ALL {
            let parsed = Cli::try_parse_from(["sb", "edit", file.command()])
                .expect("parses")
                .command;
            let Command::Edit {
                command: Some(command),
                ..
            } = parsed
            else {
                panic!("{}: not an edit subcommand", file.command());
            };
            assert_eq!(command.file(), file, "{}", file.command());
        }
    }

    /// `edit` needs root and logs each subcommand under its sb-go name,
    /// the picker as `edit`.
    #[test]
    fn edit_needs_root_and_logs_its_file() {
        let edit = |command| Command::Edit {
            command,
            no_edit_again: false,
        };
        assert!(edit(Some(EditCommand::Settings)).needs_root());
        assert!(edit(None).needs_root());
        assert_eq!(edit(None).name(), "edit");
        assert_eq!(edit(Some(EditCommand::Accounts)).name(), "edit accounts");
        assert_eq!(
            edit(Some(EditCommand::AdvSettings)).name(),
            "edit adv_settings"
        );
        assert_eq!(
            edit(Some(EditCommand::BackupConfig)).name(),
            "edit backup_config"
        );
        assert_eq!(
            edit(Some(EditCommand::HetznerVlan)).name(),
            "edit hetzner_vlan"
        );
        assert_eq!(edit(Some(EditCommand::Settings)).name(), "edit settings");
        assert_eq!(edit(Some(EditCommand::Inventory)).name(), "edit inventory");
        assert_eq!(EditCommand::Inventory.file(), edit::File::Inventory);
    }

    /// `fact` needs root (its files are root's to change) and logs under
    /// its name.
    #[test]
    fn fact_needs_root_and_logs_its_name() {
        assert!(Command::Fact.needs_root());
        assert_eq!(Command::Fact.name(), "fact");
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

    /// The run lock's commands (cli-core spec, "Run lock"): the ones that
    /// change the host take it; motd, which `pam_motd` runs at login, and
    /// the read-only and interactive ones never do.
    #[test]
    fn the_host_changing_commands_take_the_run_lock() {
        let takes = |args: &[&str]| {
            let cli = Cli::try_parse_from(std::iter::once("sb").chain(args.iter().copied()))
                .unwrap_or_else(|err| panic!("{args:?}: {err}"));
            cli.command.takes_run_lock()
        };
        for args in [
            &["install", "plex"][..],
            &["update"],
            &["setup"],
            &["gha"],
            &["branch", "develop"],
            &["branch-sandbox", "develop"],
            &["self-update"],
            &["update-venv"],
            &["reinstall-venv"],
            &["reinstall-python"],
            &["reinstall-facts"],
            &["restore"],
            &["announcements", "preview", "--run-migrations"],
        ] {
            assert!(takes(args), "{args:?}");
        }
        for args in [
            &["motd"][..],
            &["list"],
            &["logs"],
            &["docker", "ps"],
            &["docker", "start"],
            &["docker", "stop"],
            &["docker", "restart"],
            &["validate-config"],
            &["announcements"],
            &["announcements", "preview"],
            &["announcements", "check"],
            &["edit", "settings"],
            &["fact"],
            &["bench"],
            &["completion", "install"],
            &["version"],
            &["diag"],
        ] {
            assert!(!takes(args), "{args:?}");
        }
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
            // gha too: without the relaunch, `SUDO_USER` is never set.
            (Command::Gha, "gha"),
        ] {
            assert!(command.needs_root(), "{name}");
            assert_eq!(command.name(), name);
        }
        // setup changes apt, /srv and the Saltbox root (ops assumption 2).
        let setup = Cli::try_parse_from(["sb", "setup"])
            .expect("parses")
            .command;
        assert!(setup.needs_root());
        assert_eq!(setup.name(), "setup");
        // update, apt and the checkouts.
        let update = Cli::try_parse_from(["sb", "update"])
            .expect("parses")
            .command;
        assert!(update.needs_root());
        assert_eq!(update.name(), "update");
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
