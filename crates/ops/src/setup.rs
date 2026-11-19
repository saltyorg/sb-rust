//! `sb setup`, hidden (ops spec, "`sb setup` (hidden)"; sb-go's
//! cmd/setup.go and saltbox/setup.go's `SaltboxRepo`): Saltbox installed
//! on a fresh server. Before any step, the guard on the Saltbox root and,
//! over an existing checkout, the branch question; then one root step
//! `Installing Saltbox` holding sb-go's five phases in its order and
//! names, then completion's refresh (ops spec, "`sb setup`", 4.; M87):
//! a step after `Installing Saltbox` in [`run`]'s runner, a failure a
//! warning.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::Exit;
use sb_host::SetupUnsupported;
use sb_host::apt::Apt;
use sb_python_env::{Env, FactMode, Force, install_fact, reconcile_venv};
use sb_repo::{Answer, Repo};
use sb_term_ui::{Labels, PromptError, Runner, Step};

pub use crate::branch::{BRANCH_FLAGS, BranchFlags, DISCARD_FLAG};
use crate::completions::{self, Refresh};
use crate::discard::{self, DiscardFlag, Purpose};
use crate::locale::Locale;
use crate::prereq::Dirs;
use crate::{branch, configs, hooks, locale, prereq};

/// Saltbox's repository (sb-go's `layout.SaltboxRepoURL`).
pub const SALTBOX_URL: &str = "https://github.com/saltyorg/saltbox.git";

/// `-b`'s default, and the branch an empty `-b` stands for (sb-go's
/// `SaltboxRepo`).
pub const DEFAULT_BRANCH: &str = "master";

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
    #[command(flatten)]
    pub discard: DiscardFlag,
    /// Enable verbose output
    // Accepted, hidden and ignored, with a run-log line, until the served
    // installer stops passing it (ops decision 1, difference 1).
    #[arg(short, long, global = true, hide = true)]
    pub verbose: bool,
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
    /// repo's branch notices for the choice, then the local changes
    /// decision's, shown under the root step.
    pub notices: Vec<String>,
    /// Whether a checkout is fetched and reset; false when its local
    /// changes question was declined (decision 42), which leaves it as
    /// it is.
    pub reset: bool,
}

/// The plan for `found` (the guard's) at `root`, `-b`'s `wanted` branch
/// ([`DEFAULT_BRANCH`] when empty): over a directory, `<root>/.git`
/// decides whether it is a checkout (sb-go's `os.Stat`). On a checkout,
/// the branch question ([`branch::resolve`]: `answer`, the flags', or
/// else `ask`), then its local changes for that branch (decision 42:
/// `discard_local`, else `ask`; declined, the checkout isn't reset).
/// Elsewhere the plan is `wanted`, which the clone or the init checks.
/// `ask` gets the context, the question and the flag that answers it.
///
/// # Errors
///
/// `inspect existing Saltbox Git repository: <cause>` when `.git` can't
/// be inspected; the checkout's branch unreadable; a question unanswered
/// (no terminal: term-ui's error naming [`BRANCH_FLAGS`], or the refusal
/// naming the local changes and [`DISCARD_FLAG`]); a refused branch name.
pub async fn plan(
    root: &Path,
    found: Found,
    wanted: &str,
    answer: Option<Answer>,
    discard_local: bool,
    mut ask: impl FnMut(&str, &str, &'static str) -> Result<bool, PromptError>,
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
            reset: true,
        });
    }
    let repo = Repo::open(crate::Checkout::Saltbox.name(), root)?;
    let current = repo.current_branch(&shutdown.token()).await?;
    let branch::Resolved {
        branch,
        mut notices,
    } = branch::resolve(
        &repo,
        &current,
        wanted,
        answer,
        |shown: &str, question: &str| ask(shown, question, BRANCH_FLAGS),
        shutdown,
    )
    .await?;
    let doing = format!("updating to '{branch}'");
    let purpose = Purpose {
        doing: &doing,
        then: "update",
        declined: "the rest of setup goes on",
        work: "setup",
        flags: DISCARD_FLAG,
    };
    let decision = discard::decide(
        &repo,
        &current,
        &branch,
        discard_local,
        purpose,
        ask,
        shutdown,
    )
    .await?;
    // Left as it is, the branch's notice would say what didn't happen:
    // the answer alone is noted.
    if !decision.update {
        notices.clear();
    }
    notices.extend(decision.notice);
    Ok(Plan {
        found,
        branch,
        notices,
        reset: decision.update,
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
    /// The completion refresh after the installation (step 4).
    pub completions: Refresh<'a>,
}

/// setup under one root step `Installing Saltbox` (success `Saltbox
/// installation completed`): `plan`'s notices, then the phases in sb-go's
/// order, each `<name>` with success `<name> completed`, each part's
/// error in sb-go's words around the part's own; then, when it
/// succeeded, the completion refresh as a step of its own
/// ([`completions::refresh`], a failure a warning), as sb-go's setup
/// runs it after its task.
///
/// # Errors
///
/// The step's quiet `Exit` when a phase failed (its block shown under
/// it), or the completion refresh's when Ctrl-C or a signal stops it.
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
        .await?;
    completions::refresh(runner, &setup.completions).await
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
            let _repo = Repo::clone(
                crate::Checkout::Saltbox.name(),
                setup.root,
                setup.url,
                &plan.branch,
                step,
            )
            .await?;
        }
        Found::Directory => {
            Repo::open(crate::Checkout::Saltbox.name(), setup.root)?
                .init_from_remote(setup.url, &plan.branch, step)
                .await?;
        }
        Found::Checkout if plan.reset => {
            let repo = Repo::open(crate::Checkout::Saltbox.name(), setup.root)?;
            branch::update(step, &repo, &plan.branch, "root").await?;
        }
        // Its local changes question declined (decision 42): left as it
        // is, the rest of the phase runs.
        Found::Checkout => {
            tracing::info!(repo = "Saltbox", reason = "declined", "git skipped");
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
