//! `sb update`, its first half (ops spec, "`sb update`", steps 1 to 5, 7
//! and 9; sb-go's cmd/update.go; module build M84): before anything
//! changes, the checkouts' branches read, the up-front flag check and the
//! disk space; then self-update, the branch questions, apt, and Saltbox
//! and Sandbox each fetched and reset to their branch. The runtime, the
//! tags caches, the config permissions, announcements and migrations,
//! validation and the completion refresh join in later tasks (M85, M87).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::Exit;
use sb_host::apt::Apt;
use sb_host::disk::Disk;
use sb_release::{Outcome, Releases, SelfUpdate};
use sb_repo::{Answer, Choice, Repo, branch_choice};
use sb_term_ui::{Labels, PromptError, Runner, Step};

use crate::branch::{self, Resolved};
pub use crate::branch::{BRANCH_FLAGS, BranchFlags};

/// The branch `update` resets to, unless the user keeps another (sb-go's
/// `"master"`).
pub const DEFAULT_BRANCH: &str = "master";

/// Self-update's warning after it replaced sb (sb-go's).
pub const RERUN: &str = "Re-run the update command to update Saltbox";

/// `sb update`'s flags: sb-go's but `-v` (ops decision 18), persistent as
/// there (`global`), and `--migrations` (decision 19).
#[derive(clap::Args, Debug, Clone)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub branch_flags: BranchFlags,
    /// Skip CLI self-update check
    #[arg(long, global = true)]
    pub skip_self_update: bool,
    /// Run or skip the migrations new announcements name, without asking
    // Required without a terminal: its question comes after the checkouts
    // have moved, and the next run can't ask it again (ops decision 19).
    #[arg(long, global = true, value_enum, value_name = "run|skip")]
    pub migrations: Option<Migrations>,
}

/// The answer to the migrations question.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Migrations {
    /// Run every migration tag the new announcements name.
    Run,
    /// Run none, naming them in a warning.
    Skip,
}

/// The up-front check (ops spec, "`sb update`", step 1; decided.md D-Q4):
/// on a terminal (`no_terminal` is `None`) nothing is needed. Without one
/// (`no_terminal` says why, term-ui's `can_ask`'s reason), a branch flag
/// when a checkout is off master (`off_master`: each checkout's name and
/// branch) and `--migrations` always.
///
/// # Errors
///
/// One text naming each missing flag and why, after why no question can
/// be asked.
pub fn missing_answers(
    no_terminal: Option<&str>,
    off_master: &[(&str, &str)],
    branch: Option<Answer>,
    migrations: Option<Migrations>,
) -> Result<(), String> {
    let Some(reason) = no_terminal else {
        return Ok(());
    };
    let mut missing = Vec::new();
    if branch.is_none() && !off_master.is_empty() {
        let why: Vec<String> = off_master
            .iter()
            .map(|(name, current)| {
                format!("{name} is on branch '{current}', not '{DEFAULT_BRANCH}'")
            })
            .collect();
        missing.push(format!("{BRANCH_FLAGS} ({})", why.join("; ")));
    }
    if migrations.is_none() {
        missing.push(
            "--migrations run or --migrations skip (its question comes after the checkouts \
             have moved, and the next run can't ask it again)"
                .to_owned(),
        );
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "can't ask sb update's questions: {reason}; answer them with {}",
        missing.join(", and ")
    ))
}

/// What update runs with.
#[derive(Debug)]
pub struct Update<'a> {
    /// The Saltbox checkout, the Saltbox root.
    pub saltbox: &'a Path,
    /// The Sandbox checkout, `<appdata>/sandbox`.
    pub sandbox: &'a Path,
    /// host's disk checks, and the paths they check (`/`, the appdata
    /// path, `/srv`).
    pub disk: &'a Disk,
    pub disk_paths: &'a [PathBuf],
    /// host's apt.
    pub apt: &'a Apt,
    /// The release client for self-update.
    pub releases: &'a Releases,
    /// Self-update with its answer given; `None` for `--skip-self-update`.
    pub self_update: Option<SelfUpdate<'a>>,
    /// The flags.
    pub args: &'a UpdateArgs,
}

/// A checkout opened and its branch read, before any step.
struct Opened {
    repo: Repo,
    current: String,
}

/// `sb update`'s first half, in sb-go's order with the spec's changes:
///
/// 1. both checkouts opened and their branches read, then
///    [`missing_answers`] with `terminal` (term-ui's `can_ask`) for the
///    checkouts that could be read;
/// 2. host's disk check of `disk_paths`;
/// 3. self-update (a runner of its own from `runner`): replaced, the run
///    ends here with a requested exit 0 and release's [`RERUN`] warning;
/// 4. the branch questions, Saltbox's then Sandbox's ([`branch::resolve`],
///    `ask` on the terminal, no runner drawing);
/// 5. one runner: "Updating apt package cache", then "Updating Saltbox"
///    and "Updating Sandbox", each repo's fetch and reset to its branch,
///    owned by the Saltbox user, the branch notices shown under it. A
///    checkout that couldn't be opened or read fails its own step (`error
///    updating <Repo>: ...`), after the steps before it, as sb-go's
///    `requireDirectory` does.
///
/// # Errors
///
/// A refusal before any step (its text), self-update's failure (`error
/// during self-update: ...`), an unanswered question, or a failed step's
/// quiet `Exit` (its block shown in the tree).
pub async fn run(
    update: &Update<'_>,
    terminal: Result<(), String>,
    mut ask: impl FnMut(&str, &str) -> Result<bool, PromptError>,
    mut runner: impl FnMut() -> Runner,
    shutdown: &Shutdown,
) -> anyhow::Result<()> {
    let token = shutdown.token();
    let open = async |checkout: branch::Checkout, path: &Path| -> anyhow::Result<Opened> {
        let repo = Repo::open(checkout.name(), path)?;
        let current = repo.current_branch(&token).await?;
        Ok(Opened { repo, current })
    };
    let saltbox = open(branch::Checkout::Saltbox, update.saltbox).await;
    let sandbox = open(branch::Checkout::Sandbox, update.sandbox).await;
    // Off master is what repo's `branch_choice` would ask about.
    let off_master: Vec<(&str, &str)> = [&saltbox, &sandbox]
        .into_iter()
        .filter_map(|opened| opened.as_ref().ok())
        .filter(|opened| branch_choice(&opened.current, DEFAULT_BRANCH, None) == Choice::Ask)
        .map(|opened| (opened.repo.name(), opened.current.as_str()))
        .collect();
    let answer = update.args.branch_flags.answer();
    missing_answers(
        terminal.err().as_deref(),
        &off_master,
        answer,
        update.args.migrations,
    )
    .map_err(anyhow::Error::msg)?;
    update.disk.check_disk_space(update.disk_paths)?;
    if let Some(self_update) = update.self_update {
        let outcome = sb_release::self_update(update.releases, self_update, &mut runner)
            .await
            .context("error during self-update")?;
        if matches!(outcome, Outcome::Updated(_)) {
            // The new sb isn't run in this process (sb-go 1123432): the
            // run ends successfully here, as sb-go's `Shutdown(ctx, 0)`.
            let _ = shutdown.request(0);
            return Ok(());
        }
    }
    let mut plans = Vec::new();
    for opened in [saltbox, sandbox] {
        let plan = match opened {
            Ok(opened) => {
                let resolved = branch::resolve(
                    &opened.repo,
                    &opened.current,
                    DEFAULT_BRANCH,
                    answer,
                    &mut ask,
                    shutdown,
                )
                .await
                .with_context(|| format!("error updating {}", opened.repo.name()))?;
                Ok((opened.repo, resolved))
            }
            Err(err) => Err(err),
        };
        plans.push(plan);
    }
    let mut steps = runner();
    let result = async {
        apt_cache(&mut steps, update.apt).await?;
        for (checkout, plan) in [branch::Checkout::Saltbox, branch::Checkout::Sandbox]
            .into_iter()
            .zip(plans)
        {
            checkout_step(&mut steps, checkout.name(), plan, update.saltbox).await?;
        }
        Ok::<(), Exit>(())
    }
    .await;
    steps.finish();
    Ok(result?)
}

/// "Updating apt package cache", its sub-step "Refreshing apt package
/// lists": host's `update_lists` (sb-go's `UpdatePackageLists`).
async fn apt_cache(runner: &mut Runner, apt: &Apt) -> Result<(), Exit> {
    let updating = "Updating apt package cache";
    runner
        .step(Labels::new(updating, updating), async |step| {
            let refreshing = "Refreshing apt package lists";
            step.step(Labels::new(refreshing, refreshing), async |step| {
                apt.update_lists(step)
                    .await
                    .context("error updating apt cache")
            })
            .await?;
            Ok(())
        })
        .await
}

/// "Updating <Repo>" (success "<Repo> updated"): the branch notices, the
/// Saltbox user from the Saltbox root's accounts.yml, then the checkout
/// fetched and reset to the resolved branch, owned by that user; or the
/// error that kept the checkout from being opened (sb-go's `error
/// updating <Repo>: inspect required directory ...`).
async fn checkout_step(
    runner: &mut Runner,
    name: &str,
    plan: anyhow::Result<(Repo, Resolved)>,
    root: &Path,
) -> Result<(), Exit> {
    let labels = Labels::new(format!("Updating {name}"), format!("{name} updated"))
        .failure(format!("{name} update"));
    runner
        .step(labels, async |step: &mut Step| {
            let (repo, resolved) = plan.with_context(|| format!("error updating {name}"))?;
            for notice in &resolved.notices {
                step.info(notice);
            }
            let user = sb_config::user::saltbox_user(root).context("error getting saltbox user")?;
            branch::update(step, &repo, &resolved.branch, &user).await?;
            Ok(())
        })
        .await
}
