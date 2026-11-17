//! `sb update` (ops spec, "`sb update`"; sb-go's cmd/update.go; module
//! build M84 and M85): before anything changes, the checkouts' branches
//! read, the up-front flag check and the disk space; then self-update,
//! the branch questions, apt, Saltbox (its runtime and tags), the config
//! permissions, Sandbox (its tags), the new announcements inline, their
//! migrations (M86), and validation. The viewer and the completion
//! refresh come with M87.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sb_ansible::{Ansible, RunArgs, TagCache, Tags};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{self, Exit};
use sb_cli_core::theme;
use sb_config::run::Settings;
use sb_exec::Ending;
use sb_host::apt::Apt;
use sb_host::disk::Disk;
use sb_python_env::{Env, reconcile_runtime};
use sb_release::{Outcome, Releases, SelfUpdate};
use sb_repo::{Answer, Choice, Repo, branch_choice};
use sb_term_ui::{Labels, PromptError, Runner, Step};

use crate::announcements::{self, Announcement, LoadError, Pending};
use crate::branch::{self, Resolved};
pub use crate::branch::{BRANCH_FLAGS, BranchFlags};
use crate::{Checkout, configs};

/// The branch `update` resets to, unless the user keeps another (sb-go's
/// `"master"`).
pub const DEFAULT_BRANCH: &str = "master";

/// Self-update's warning after it replaced sb (sb-go's).
pub const RERUN: &str = "Re-run the update command to update Saltbox";

/// The question before the migrations run, all or none (sb-go's).
pub const MIGRATIONS_QUESTION: &str = "Run all migration tags?";

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
    /// The Saltbox root: Saltbox's checkout, its config and tags cache;
    /// Sandbox's checkout under its inventory's appdata path
    /// ([`Checkout::path`]).
    pub root: &'a Path,
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
    /// python-env's layout, release client and programs: Saltbox's runtime.
    pub python: &'a Env<'a>,
    /// `ansible-playbook`, for the tags caches and the migrations.
    pub ansible: &'a Ansible,
    /// validate-config's settings, its root this one.
    pub settings: &'a Settings,
    /// The directory motd.yml's migration backs the original up into.
    pub backups: &'a Path,
}

/// A checkout opened and its branch read, before any step.
struct Opened {
    repo: Repo,
    current: String,
}

/// `sb update`, in sb-go's order with the spec's changes:
///
/// 1. both checkouts opened and their branches read, then
///    [`missing_answers`] with `terminal` (term-ui's `can_ask`) for the
///    checkouts that could be read;
/// 2. host's disk check of `disk_paths`;
/// 3. self-update (a runner of its own from `runner`): replaced, the run
///    ends here with a requested exit 0 and release's [`RERUN`] warning;
/// 4. the branch questions, Saltbox's then Sandbox's ([`branch::resolve`],
///    `ask` on the terminal, no runner drawing);
/// 5. one runner: "Updating apt package cache"; each checkout's
///    announcements read; "Updating Saltbox" (the fetch and reset, the
///    runtime, the tags when the commit moved or none are cached);
///    "Securing Saltbox configuration permissions"; "Updating Sandbox"
///    (the same but the runtime); "Checking for new announcements". A
///    checkout that couldn't be opened or read fails its own step (`error
///    updating <Repo>: ...`), after the steps before it, as sb-go's
///    `requireDirectory` does;
/// 6. the new announcements on stdout, once that runner is done (inline
///    until the viewer, M87), then their [`migrations`];
/// 7. a runner of its own: validate-config's checks (sb-config's `run`).
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
    confirm: impl FnOnce(&str) -> Result<bool, PromptError>,
    mut runner: impl FnMut() -> Runner,
    shutdown: &Shutdown,
    lines: &mut impl Write,
) -> anyhow::Result<()> {
    let token = shutdown.token();
    let open = async |checkout: Checkout, path: &Path| -> anyhow::Result<Opened> {
        let repo = Repo::open(checkout.name(), path)?;
        let current = repo.current_branch(&token).await?;
        Ok(Opened { repo, current })
    };
    let saltbox = open(Checkout::Saltbox, &Checkout::Saltbox.path(update.root)).await;
    let sandbox = open(Checkout::Sandbox, &Checkout::Sandbox.path(update.root)).await;
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
    let saltbox = plan(saltbox, answer, &mut ask, shutdown).await?;
    let sandbox = plan(sandbox, answer, &mut ask, shutdown).await?;
    let mut steps = runner();
    let result = async {
        apt_cache(&mut steps, update.apt).await?;
        // Each checkout's announcements before its pull (sb-go reads both
        // here, after apt).
        let before: Vec<_> = [Checkout::Saltbox, Checkout::Sandbox]
            .into_iter()
            .map(|checkout| announcements::load(&announcements::own_file(checkout, update.root)))
            .collect();
        let mut cache = TagCache::load(update.root);
        checkout_step(&mut steps, Checkout::Saltbox, saltbox, update, &mut cache).await?;
        secure_step(&mut steps, update.root).await?;
        checkout_step(&mut steps, Checkout::Sandbox, sandbox, update, &mut cache).await?;
        new_announcements(&mut steps, update.root, before).await
    }
    .await;
    steps.finish();
    let shown = result?;
    // On stdout, once the runner is done (inline on a terminal too until
    // the viewer, M87).
    if !shown.is_empty() {
        announcements::show(&shown)?;
    }
    migrations(update, &shown, confirm, shutdown, lines).await?;
    let mut steps = runner();
    let validated = sb_config::run::run(&mut steps, update.settings, update.backups).await;
    steps.finish();
    validated.context("error validating Saltbox configuration")
}

/// The migrations the new announcements `shown` name (ops spec,
/// "Migrations"; sb-go's `PromptForMigrations` and `ExecuteMigrations`),
/// all or none: their table on stdout, then `--migrations` or, without
/// it, [`MIGRATIONS_QUESTION`] (`confirm`, on the terminal; default no).
/// Run: `Starting migration execution`, then for each `Running migration
/// '<tag>' for <Repo> repository` and ansible's run of that tag in that
/// checkout's playbook, attached, its lines on stdout as `sb install`'s,
/// then `All migrations completed successfully`. Not run: a warning
/// naming each (decision 22). sb's lines go to `lines`, a stream that
/// strips colour when it is off (anstream's); the run log gets the
/// decision and each migration.
///
/// # Errors
///
/// The question unanswered; a migration that fails (`failed to execute
/// migration '<tag>' for <Repo> repository: ...`, exit 1, the ones after
/// it named as the lines that run them by hand), or is
/// interrupted (exec's quiet exit, sb then ending by the signal); `lines`
/// that can't be written.
async fn migrations(
    update: &Update<'_>,
    shown: &[(Checkout, Vec<Announcement>)],
    confirm: impl FnOnce(&str) -> Result<bool, PromptError>,
    shutdown: &Shutdown,
    lines: &mut impl Write,
) -> anyhow::Result<()> {
    let pending = announcements::migrations(shown);
    if pending.is_empty() {
        return Ok(());
    }
    announcements::show_migrations(&pending)?;
    let approved = match update.args.migrations {
        Some(Migrations::Run) => true,
        Some(Migrations::Skip) => false,
        None => confirm(MIGRATIONS_QUESTION)?,
    };
    let named = pending
        .iter()
        .map(|migration| format!("{} {}", migration.repo.name(), migration.tag))
        .collect::<Vec<_>>()
        .join(", ");
    // The run log keeps what was decided and what ran (decision 22), as
    // the runner's notices reach it; these lines aren't a runner's, since
    // the playbooks run attached.
    tracing::info!(
        migrations = %named,
        approved,
        by_flag = update.args.migrations.is_some(),
        "migrations decided"
    );
    let write = |lines: &mut dyn Write, text: &str| -> anyhow::Result<()> {
        writeln!(lines, "{text}")
            .and_then(|()| lines.flush())
            .map_err(exit::output_error)?;
        Ok(())
    };
    if !approved {
        tracing::warn!(migrations = %named, "migrations skipped");
        // sb's `warning:` line, as the binary's own: the theme's warn
        // colour, which `lines` (anstream's stderr in sb) strips when
        // colour is off.
        let style = theme::current().style(theme::Role::Warn);
        return write(
            lines,
            &format!(
                "{style}warning:{style:#} {}",
                exit::escaped(&format!("Skipped migrations: {named}"))
            ),
        );
    }
    write(lines, "Starting migration execution")?;
    for (at, migration) in pending.iter().enumerate() {
        let name = migration.repo.name();
        let tag = &migration.tag;
        tracing::info!(repo = name, tag = %tag, "migration started");
        write(
            lines,
            &exit::escaped(&format!("Running migration '{tag}' for {name} repository")),
        )?;
        let tags = [tag.clone()];
        // A failure names the migrations after it, which the next update
        // won't offer again (the announcements are no longer new): each as
        // the line that runs it by hand (difference 41).
        let context = || {
            let left = by_hand(&pending[at + 1..]);
            format!("failed to execute migration '{tag}' for {name} repository{left}")
        };
        // ansible's lines on stdout, as `sb install`'s.
        let ran = update
            .ansible
            .run(
                &migration.repo.playbooks(update.root),
                Tags::Given(&tags),
                &RunArgs::default(),
                shutdown,
                &mut io::stdout(),
            )
            .await
            .with_context(context)?;
        // Exec's quiet exit for an interrupted run (its `pass_through`):
        // cli-core ends sb by the signal.
        if ran.outcome.ending == Ending::Interrupted {
            return Err(Exit(1).into());
        }
        let _ran = ran.check().with_context(context)?;
        tracing::info!(repo = name, tag = %tag, "migration completed");
    }
    write(lines, "All migrations completed successfully")
}

/// The migrations `left` as the `sb install` lines that run them by
/// hand, in parentheses after ` (not run, to run by hand: `, so the cause
/// shown after the failure isn't read as theirs; nothing when none are
/// left: Saltbox's tag as it is, Sandbox's with its `sandbox-` prefix,
/// as `sb install` routes tags (ansible spec).
fn by_hand(left: &[Pending]) -> String {
    if left.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = left
        .iter()
        .map(|migration| match migration.repo {
            Checkout::Saltbox => format!("sb install {}", migration.tag),
            Checkout::Sandbox => format!("sb install sandbox-{}", migration.tag),
        })
        .collect();
    format!(" (not run, to run by hand: {})", lines.join(", "))
}

/// The branch question for an opened checkout ([`branch::resolve`]): the
/// outer error ends the run before any step (an unanswered question); the
/// inner one is the checkout's that couldn't be opened, kept for its own
/// step.
///
/// # Errors
///
/// The question unanswered, or a refused branch name (`error updating
/// <Repo>: ...`).
async fn plan(
    opened: anyhow::Result<Opened>,
    answer: Option<Answer>,
    ask: impl FnOnce(&str, &str) -> Result<bool, PromptError>,
    shutdown: &Shutdown,
) -> anyhow::Result<anyhow::Result<(Repo, Resolved)>> {
    let opened = match opened {
        Ok(opened) => opened,
        Err(err) => return Ok(Err(err)),
    };
    let resolved = branch::resolve(
        &opened.repo,
        &opened.current,
        DEFAULT_BRANCH,
        answer,
        ask,
        shutdown,
    )
    .await
    .with_context(|| format!("error updating {}", opened.repo.name()))?;
    Ok(Ok((opened.repo, resolved)))
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

/// "Updating <Repo>" (success "<Repo> updated"), sb-go's sub-steps: the
/// branch notices, the Saltbox user from the Saltbox root's accounts.yml,
/// the HEAD commit, the checkout fetched and reset to the resolved branch
/// and owned by that user, for Saltbox python-env's `reconcile_runtime`
/// (deadsnakes, venv, fact, as `update-venv`), the HEAD commit again, and
/// the tags cache rebuilt when the commit moved or the cache has no tags
/// for the checkout. A checkout that couldn't be opened fails here with
/// its error (sb-go's `error updating <Repo>: inspect required directory
/// ...`).
async fn checkout_step(
    runner: &mut Runner,
    checkout: Checkout,
    plan: anyhow::Result<(Repo, Resolved)>,
    update: &Update<'_>,
    cache: &mut TagCache,
) -> Result<(), Exit> {
    let name = checkout.name();
    let labels = Labels::new(format!("Updating {name}"), format!("{name} updated"))
        .failure(format!("{name} update"));
    runner
        .step(labels, async |step: &mut Step| {
            let (repo, resolved) = plan.with_context(|| format!("error updating {name}"))?;
            for notice in &resolved.notices {
                step.info(notice);
            }
            let token = step.token();
            let user =
                sb_config::user::saltbox_user(update.root).context("error getting saltbox user")?;
            let old = repo
                .head(&token)
                .await
                .context("error getting old commit hash")?;
            branch::update(step, &repo, &resolved.branch, &user).await?;
            if checkout == Checkout::Saltbox {
                reconcile_runtime(step, update.python, Some(&user)).await?;
            }
            let new = repo
                .head(&token)
                .await
                .context("error getting new commit hash")?;
            let playbooks = checkout.playbooks(update.root);
            // sb-go's rule for update (ops spec, step 7): rebuilt when the
            // commit moved or no tags are cached. Why not `TagCache::tags`,
            // which also rebuilds an entry cached at another commit: that
            // isn't sb-go's update rule.
            let cached = cache
                .entry(&playbooks)
                .is_some_and(|entry| !entry.tags.is_empty());
            if old != new || !cached {
                branch::tags(step, cache, update.ansible, &playbooks, &repo).await?;
            }
            Ok(())
        })
        .await
}

/// "Securing Saltbox configuration permissions" (success "Saltbox
/// configuration permissions secured"): ops' `secure_existing_configs`,
/// sb-go's step after Saltbox's.
async fn secure_step(runner: &mut Runner, root: &Path) -> Result<(), Exit> {
    let labels = Labels::new(
        "Securing Saltbox configuration permissions",
        "Saltbox configuration permissions secured",
    )
    .failure("Saltbox configuration permission migration");
    runner
        .step(labels, async |_| {
            configs::secure_existing_configs(root)
                .context("error securing Saltbox configuration permissions")
        })
        .await
}

/// "Checking for new announcements" (success "Checked for new
/// announcements"): each checkout's announcements after its pull against
/// `before`, the new ones that apply, by date (ops spec,
/// "Announcements"), Saltbox's first, with sb-go's info `Displaying new
/// announcements` when there are any. A file that can't be loaded, before
/// or after, is a warning naming it, and that checkout's announcements are
/// skipped this run (decision 23); a missing file is none.
///
/// # Errors
///
/// The step's quiet `Exit` (a step that can't fail otherwise).
async fn new_announcements(
    runner: &mut Runner,
    root: &Path,
    before: Vec<Result<Option<Vec<Announcement>>, LoadError>>,
) -> Result<Vec<(Checkout, Vec<Announcement>)>, Exit> {
    let labels = Labels::new(
        "Checking for new announcements",
        "Checked for new announcements",
    );
    runner
        .step(labels, async |step| {
            let mut shown = Vec::new();
            for (checkout, before) in [Checkout::Saltbox, Checkout::Sandbox].into_iter().zip(before) {
                let path = announcements::own_file(checkout, root);
                let skipped = |step: &mut Step, which: &str, err: &LoadError| {
                    step.warn(format!(
                        "{} announcements skipped: error loading {which} announcement file '{}': {err}",
                        checkout.name(),
                        path.display()
                    ));
                };
                let before = match before {
                    Ok(before) => before,
                    Err(err) => {
                        skipped(step, "before", &err);
                        continue;
                    }
                };
                let after = match announcements::load(&path) {
                    Ok(after) => after,
                    Err(err) => {
                        skipped(step, "after", &err);
                        continue;
                    }
                };
                let new = announcements::new_entries(before.as_deref(), after.as_deref());
                tracing::info!(repo = checkout.name(), new = new.len(), "announcements compared");
                if !new.is_empty() {
                    shown.push((checkout, new));
                }
            }
            if !shown.is_empty() {
                step.info("Displaying new announcements");
            }
            Ok(shown)
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(repo: Checkout, tag: &str) -> Pending {
        Pending {
            repo,
            tag: tag.to_owned(),
            date: "2026-11-17".to_owned(),
        }
    }

    /// Each migration left unrun as the `sb install` line that runs it:
    /// routed back through `sb install`'s own rule (sb-ansible's
    /// `Routed::route`), each line lands in its checkout with its tag.
    #[test]
    fn unrun_migrations_are_the_lines_that_run_them() {
        assert_eq!(by_hand(&[]), "");
        let left = [
            pending(Checkout::Saltbox, "migrate-a"),
            pending(Checkout::Sandbox, "migrate-c"),
        ];
        let text = by_hand(&left);
        let inside = text
            .strip_prefix(" (not run, to run by hand: ")
            .and_then(|rest| rest.strip_suffix(')'))
            .expect("the parenthesised list");
        let tags: Vec<String> = inside
            .split(", ")
            .map(|line| {
                line.strip_prefix("sb install ")
                    .expect("an sb install line")
                    .to_owned()
            })
            .collect();
        let routed = sb_ansible::Routed::route(&tags);
        assert_eq!(routed.saltbox, ["migrate-a"]);
        assert_eq!(routed.sandbox, ["migrate-c"]);
        assert_eq!(routed.saltbox_mod, Vec::<String>::new());
    }
}
