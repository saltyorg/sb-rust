//! `sb setup`, hidden (ops spec, "`sb setup` (hidden)"; sb-go's
//! cmd/setup.go and saltbox/setup.go's `SaltboxRepo`): Saltbox installed
//! on a fresh server. Before any step, the guard on the Saltbox root and,
//! over an existing checkout, the branch question; then one root step
//! `Installing Saltbox` holding sb-go's five phases in its order and
//! names. The completion refresh after it comes with completion (module
//! build M87).

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::Exit;
use sb_host::SetupUnsupported;
use sb_host::apt::Apt;
use sb_python_env::{Env, FactMode, Force, install_fact, reconcile_venv};
use sb_repo::{Answer, Choice, Repo, branch_choice, branch_notices};
use sb_term_ui::{Labels, PromptError, Runner, Step};

use crate::locale::Locale;
use crate::prereq::Dirs;
use crate::{branch, configs, hooks, locale, prereq};

/// Saltbox's repository (sb-go's `layout.SaltboxRepoURL`).
pub const SALTBOX_URL: &str = "https://github.com/saltyorg/saltbox.git";

/// The flags that answer the branch question, as its error names them
/// (ops decision 29).
pub const BRANCH_FLAGS: &str = "--keep-branch or --reset-branch";

/// `-b`'s default, and the branch an empty `-b` stands for (sb-go's
/// `SaltboxRepo`).
pub const DEFAULT_BRANCH: &str = "master";

/// The checkout's name in labels, notices and errors.
const SALTBOX: &str = "Saltbox";

/// `sb setup`'s flags: sb-go's, persistent as there (`global`), with
/// `update`'s two branch flags (ops decision 29).
#[derive(clap::Args, Debug, Clone)]
pub struct SetupArgs {
    /// Branch to use for Saltbox repository
    #[arg(short, long, global = true, default_value = DEFAULT_BRANCH)]
    pub branch: String,
    /// Allow setup to run when an existing Saltbox installation is present
    #[arg(long, global = true)]
    pub force: bool,
    #[command(flatten)]
    pub branch_flags: BranchFlags,
    /// Enable verbose output
    // Accepted, hidden and ignored, with a run-log line, until the served
    // installer stops passing it (ops decision 1, difference 1).
    #[arg(short, long, global = true, hide = true)]
    pub verbose: bool,
}

/// The flags that answer the branch question, as `update`'s (ops decision
/// 29): flattened into [`SetupArgs`]
/// (`#[command(flatten)]`, <https://docs.rs/clap/4.6.7/clap/_derive/index.html>).
#[derive(clap::Args, Debug, Clone, Copy)]
pub struct BranchFlags {
    /// Skip branch reset prompt and stay on current branch
    // Both together are clap's parse error, exit 2 (ops difference 33):
    // <https://docs.rs/clap/4.6.7/clap/struct.Arg.html#method.conflicts_with>.
    #[arg(long, global = true, conflicts_with = "reset_branch")]
    pub keep_branch: bool,
    /// Skip branch reset prompt and reset to default branch
    #[arg(long, global = true)]
    pub reset_branch: bool,
}

impl BranchFlags {
    /// The branch question's answer the flags give, if any.
    #[must_use]
    pub fn answer(self) -> Option<Answer> {
        if self.keep_branch {
            Some(Answer::Keep)
        } else if self.reset_branch {
            Some(Answer::Reset)
        } else {
            None
        }
    }
}

/// Why the guard refuses the Saltbox root, in sb-go's words
/// (`checkSetupAllowed`, cmd/setup.go).
#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    /// The root is a symlink, refused even with `--force`.
    #[error("{} exists but is a symlink", .0.display())]
    Symlink(PathBuf),
    /// The root isn't a directory, refused even with `--force`.
    #[error("{} exists but is not a directory", .0.display())]
    NotDirectory(PathBuf),
    /// The root is a directory and `--force` wasn't given.
    #[error(
        "setup already exists at {}; setup is intended for fresh installations only (use --force to run it anyway)",
        .0.display()
    )]
    Exists(PathBuf),
    /// The root couldn't be inspected (Go's `lstat <path>: <cause>`).
    #[error("inspect existing Saltbox installation: lstat {}: {cause}", path.display())]
    Inspect { path: PathBuf, cause: io::Error },
}

/// What setup finds at the Saltbox root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// Nothing: setup clones.
    Nothing,
    /// A directory without `.git` (only with `--force`): setup makes it a
    /// checkout.
    Directory,
    /// A checkout (`<root>/.git` exists; only with `--force`): setup
    /// fetches and resets it.
    Checkout,
}

/// The guard (sb-go's `checkSetupAllowed`): `lstat` of `root`, so a
/// symlink is seen as one. Missing is [`Found::Nothing`]; a directory is
/// [`Found::Directory`] with `force`. `--force` allows exactly that, never
/// a symlink or a non-directory.
///
/// # Errors
///
/// [`GuardError`], each with sb-go's text.
pub fn guard(root: &Path, force: bool) -> Result<Found, GuardError> {
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => return Ok(Found::Nothing),
        Err(cause) => {
            return Err(GuardError::Inspect {
                path: root.to_path_buf(),
                cause,
            });
        }
    };
    let kind = metadata.file_type();
    if kind.is_symlink() {
        return Err(GuardError::Symlink(root.to_path_buf()));
    }
    if !kind.is_dir() {
        return Err(GuardError::NotDirectory(root.to_path_buf()));
    }
    if !force {
        return Err(GuardError::Exists(root.to_path_buf()));
    }
    Ok(Found::Directory)
}

/// What setup runs with, decided before any step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// What the root holds.
    pub found: Found,
    /// The branch setup clones, initialises or resets to.
    pub branch: String,
    /// repo's branch notices for the choice, shown under the root step.
    pub notices: Vec<String>,
}

/// The plan for `found` (the guard's) at `root`, `-b`'s `wanted` branch
/// ([`DEFAULT_BRANCH`] when empty): over a directory, `<root>/.git`
/// decides whether it is a checkout (sb-go's `os.Stat`). On a checkout,
/// repo's `branch_choice` compares its branch with `wanted`, `answer`
/// (the flags') answers, or else `ask` shows repo's `Saltbox: Currently
/// on branch '<b>'` and asks `Saltbox: Do you want to reset to the
/// '<wanted>' branch?` (term-ui's confirm in sb, on the terminal), in
/// sb-go's order; the notices then leave out the line already shown.
/// The chosen branch must pass repo's `check_branch_name` (a kept
/// detached HEAD doesn't). Elsewhere the plan is `wanted`, which the
/// clone or the init checks.
///
/// # Panics
///
/// Never: repo's `branch_choice` asks only when no answer is given, and
/// the second call has one (a bug otherwise, M-PANIC-ON-BUG).
///
/// # Errors
///
/// `inspect existing Saltbox Git repository: <cause>` when `.git` can't
/// be inspected; the checkout's branch unreadable; the question
/// unanswered (no terminal: term-ui's error naming [`BRANCH_FLAGS`]); a
/// refused branch name.
pub async fn plan(
    root: &Path,
    found: Found,
    wanted: &str,
    answer: Option<Answer>,
    ask: impl FnOnce(&str, &str) -> Result<bool, PromptError>,
    shutdown: &Shutdown,
) -> anyhow::Result<Plan> {
    let wanted = if wanted.is_empty() {
        DEFAULT_BRANCH
    } else {
        wanted
    };
    let found = match found {
        Found::Directory => match std::fs::metadata(root.join(".git")) {
            Ok(_) => Found::Checkout,
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => Found::Directory,
            Err(cause) => {
                return Err(
                    anyhow::Error::new(cause).context("inspect existing Saltbox Git repository")
                );
            }
        },
        other => other,
    };
    if found != Found::Checkout {
        return Ok(Plan {
            found,
            branch: wanted.to_owned(),
            notices: Vec::new(),
        });
    }
    let repo = Repo::open(SALTBOX, root)?;
    let token = shutdown.token();
    let current = repo.current_branch(&token).await?;
    // Asked, repo's line for the current branch comes first, as sb-go
    // prints it before its question.
    let shown = branch_notices(&repo, &current, wanted, &Choice::Ask);
    let (choice, asked) = match branch_choice(&current, wanted, answer) {
        Choice::Ask => {
            let question = format!("{SALTBOX}: Do you want to reset to the '{wanted}' branch?");
            let answer = if ask(&shown.join("\n"), &question)? {
                Answer::Reset
            } else {
                Answer::Keep
            };
            (branch_choice(&current, wanted, Some(answer)), true)
        }
        choice => (choice, false),
    };
    let mut notices = branch_notices(&repo, &current, wanted, &choice);
    if asked {
        notices.retain(|notice| !shown.contains(notice));
    }
    let branch = match choice {
        Choice::Use(branch) | Choice::Keep(branch) => branch,
        Choice::Ask => unreachable!("branch_choice asked with an answer given"),
    };
    repo.check_branch_name(&branch, &token).await?;
    Ok(Plan {
        found,
        branch,
        notices,
    })
}

/// What setup's steps run with.
#[derive(Debug)]
pub struct Setup<'a> {
    /// host's architecture, LXC and desktop checks.
    pub checks: &'a sb_host::setup::Setup,
    /// The release check (host's `check_setup_support`; a stand-in in
    /// tests, M-MOCKABLE-SYSCALLS).
    pub support: fn() -> Result<(), SetupUnsupported>,
    /// host's apt, for the prerequisites.
    pub apt: &'a Apt,
    /// `/srv/git` and `/srv/ansible`.
    pub dirs: &'a Dirs,
    /// The locale programs and file.
    pub locale: &'a Locale,
    /// The program the git hooks run with.
    pub bash: &'a Path,
    /// The Saltbox root (`/srv/git/saltbox`).
    pub root: &'a Path,
    /// The repository cloned or fetched ([`SALTBOX_URL`]; a local one in
    /// tests).
    pub url: &'a str,
    /// python-env's layout, release client and programs.
    pub python: &'a Env<'a>,
    /// The run log's id, naming the apt sources' backup.
    pub run_id: &'a str,
}

/// setup under one root step `Installing Saltbox` (success `Saltbox
/// installation completed`): `plan`'s notices, then the phases in sb-go's
/// order, each `<name>` with success `<name> completed`, each part's
/// error in sb-go's words around the part's own.
///
/// # Errors
///
/// The step's quiet `Exit` when a phase failed (its block shown under
/// it).
pub async fn run(runner: &mut Runner, setup: &Setup<'_>, plan: &Plan) -> Result<(), Exit> {
    let labels = Labels::new("Installing Saltbox", "Saltbox installation completed")
        .failure("Saltbox installation");
    runner
        .step(labels, async |step| {
            for notice in &plan.notices {
                step.info(notice);
            }
            phase(step, "Checking system compatibility", async |step| {
                checks(step, setup).await
            })
            .await?;
            phase(step, "Installing system prerequisites", async |step| {
                prereq::prerequisites(step, setup.apt, setup.dirs, setup.run_id)
                    .await
                    .context("error during initial setup")
            })
            .await?;
            phase(step, "Configuring system locale", async |step| {
                locale::configure(step, setup.locale)
                    .await
                    .context("error configuring locale")
            })
            .await?;
            phase(step, "Preparing Saltbox repository", async |step| {
                saltbox_repo(step, setup, plan)
                    .await
                    .context("error setting up Saltbox repository")?;
                hooks::git_hooks(step, setup.root, setup.bash)
                    .await
                    .context("error initializing Git hooks")
            })
            .await?;
            phase(step, "Installing Python runtime", async |step| {
                let user = sb_config::user::saltbox_user(setup.root).context("get Saltbox user")?;
                reconcile_venv(step, setup.python, Some(&user), Force::None).await?;
                Ok(())
            })
            .await?;
            Ok(())
        })
        .await
}

/// A phase: `name`, success `<name> completed`, failure `name` (sb-go's
/// `runSetupPhase`).
async fn phase(
    step: &mut Step,
    name: &str,
    body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<()>,
) -> Result<(), Exit> {
    let labels = Labels::new(name, format!("{name} completed")).failure(name);
    step.step(labels, body).await
}

/// The compatibility checks, each a step with sb-go's label and the host
/// spec's text.
async fn checks(step: &mut Step, setup: &Setup<'_>) -> anyhow::Result<()> {
    let check = |label: &str| Labels::new(label, label);
    step.step(check("Checking Ubuntu version"), async |_| {
        Ok((setup.support)()?)
    })
    .await?;
    step.step(check("Checking CPU architecture"), async |_| {
        Ok(setup.checks.check_architecture()?)
    })
    .await?;
    step.step(check("Checking for LXC container"), async |step| {
        Ok(setup.checks.check_lxc(&step.token()).await?)
    })
    .await?;
    step.step(check("Checking for desktop environment"), async |step| {
        Ok(setup.checks.check_desktop(&step.token()).await?)
    })
    .await?;
    Ok(())
}

/// The checkout as `plan` found it (sb-go's `SaltboxRepo`): cloned at the
/// branch, made a checkout of it, or fetched and reset to it, owned by
/// root as sb-go's; then saltbox.fact and the default configs.
async fn saltbox_repo(step: &mut Step, setup: &Setup<'_>, plan: &Plan) -> anyhow::Result<()> {
    match plan.found {
        Found::Nothing => {
            let _repo = Repo::clone(SALTBOX, setup.root, setup.url, &plan.branch, step).await?;
        }
        Found::Directory => {
            Repo::open(SALTBOX, setup.root)?
                .init_from_remote(setup.url, &plan.branch, step)
                .await?;
        }
        Found::Checkout => {
            let repo = Repo::open(SALTBOX, setup.root)?;
            branch::update(step, &repo, &plan.branch, "root").await?;
        }
    }
    let fact = Labels::new("Checking saltbox.fact", "saltbox.fact is ready")
        .failure("saltbox.fact update");
    step.step(fact, async |step| {
        install_fact(step, setup.python, FactMode::Update).await?;
        Ok(())
    })
    .await
    .context("error downloading and installing saltbox.fact")?;
    configs::copy_default_configs(step, setup.root)
        .await
        .context("error copying default configuration files")
}
