//! `sb update` (ops spec, "`sb update`"; sb-go's cmd/update.go; module
//! build M84 and M85): before anything changes, the checkouts' branches
//! read, the up-front flag check and the disk space; then self-update,
//! the branch questions, apt, Saltbox (its runtime and tags), the config
//! permissions, Sandbox (its tags), the new announcements (tui's viewer
//! on a terminal, inline piped; M87), their migrations (M86), and
//! validation, then completion's refresh (M87).

use std::cell::LazyCell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sb_ansible::{Ansible, RunArgs, StartedListing, TagCache, Tags};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{self, Exit};
use sb_cli_core::theme;
use sb_config::run::Settings;
use sb_exec::Ending;
use sb_host::apt::Apt;
use sb_host::disk::Disk;
use sb_python_env::{Env, reconcile_runtime};
use sb_release::{Outcome, Releases, SelfUpdate};
use sb_repo::{Answer, Choice, Discards, Repo, branch_choice};
use sb_term_ui::{Labels, PromptError, Runner, Step};

use crate::announcements::{self, Announcement, Pending};
use crate::branch::{self, Resolved};
pub use crate::branch::{BRANCH_FLAGS, BranchFlags};
use crate::completions::{self, Refresh};
use crate::discard::{self, Decision, DiscardFlag, Purpose};
use crate::moved::{self, Left, Moved, TagDiff};
use crate::{Checkout, configs};

/// The branch `update` resets to, unless the user keeps another (sb-go's
/// `"master"`).
pub const DEFAULT_BRANCH: &str = "master";

/// Self-update's warning after it replaced sb (sb-go's).
pub const RERUN: &str = "Re-run the update command to update Saltbox";

/// The question before the migrations run, all or none (sb-go's).
pub const MIGRATIONS_QUESTION: &str = "Run all migration tags?";

/// The flags that answer the local changes question (ops decision 40).
pub const DISCARD_FLAGS: &str = "--discard-local or --skip-git";

/// The announcements step's ending under `--skip-git` (ops decision 41).
pub const NOT_COMPARED: &str = "Announcements not compared (--skip-git)";

/// Its one notice: where to see them.
pub const SEE_THEM: &str = "To see them: sb announcements preview --since <ref>";

/// The local changes question's words in update, for the update to
/// `doing` (`updating to 'master'`).
fn purpose(doing: &str) -> Purpose<'_> {
    Purpose {
        doing,
        then: "update",
        declined: "the rest of the update goes on",
        work: "update",
        flags: DISCARD_FLAGS,
    }
}

// `sb update`'s flags: sb-go's but `-v` (ops decision 18), persistent as
// there (`global`), and `--migrations` (decision 19).
// Not a doc comment: clap's derive would take it as the command's
// description, over the variant's (sb's `Command` defers it).
#[derive(clap::Args, Debug, Clone)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub branch_flags: BranchFlags,
    /// Skip CLI self-update check
    #[arg(long, global = true)]
    pub skip_self_update: bool,
    #[command(flatten)]
    pub discard: DiscardFlag,
    /// Leave both checkouts as they are and update everything else
    // Hidden, for developers (ops decision 41): no fetch, reset, clean,
    // checkout, submodule update or branch question. With a branch flag
    // or `--discard-local`, clap's parse error, exit 2:
    // <https://docs.rs/clap/4.6.7/clap/struct.Arg.html#method.conflicts_with_all>.
    #[arg(
        long,
        global = true,
        hide = true,
        conflicts_with_all = ["keep_branch", "reset_branch", "discard_local"]
    )]
    pub skip_git: bool,
    /// Run or skip the migrations new announcements name, without asking
    // Required without a terminal: its question comes after the checkouts
    // have moved, and the next run can't ask it again (ops decision 19).
    #[arg(long, global = true, value_enum, value_name = "run|skip")]
    pub migrations: Option<Migrations>,
    /// Check the Ansible virtual environment's files against their
    /// records, and its Python against sb's manifest; rebuild what differs
    // Hidden while its cost is tried on real hosts (python-env decision
    // 25): unhiding it is dropping `hide`.
    #[arg(long, global = true, hide = true)]
    pub verify_venv: bool,
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
/// branch), `--migrations` always, and [`DISCARD_FLAGS`] when a checkout
/// has local changes its update would discard (`dirty`: each checkout's
/// name and repo's summary of them; decision 40).
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
    dirty: &[(&str, String)],
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
    if !dirty.is_empty() {
        missing.push(discard::dirty_flags(dirty, "update", DISCARD_FLAGS));
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
    /// How many checkouts' git phases run at once (ops spec, step 7);
    /// `None` is host's rule, one per physical core sb may run on.
    pub git_at_once: Option<std::num::NonZeroUsize>,
    /// validate-config's settings, its root this one.
    pub settings: &'a Settings,
    /// The directory motd.yml's migration backs the original up into.
    pub backups: &'a Path,
    /// Whether stdout is a terminal: the new announcements in tui's
    /// viewer then, else inline (ops spec, "Shown").
    pub stdout_terminal: bool,
    /// The completion refresh after validation (step 12).
    pub completions: Refresh<'a>,
}

/// A checkout opened and its branch read, before any step.
struct Opened {
    repo: Repo,
    current: String,
}

/// `sb update`, in sb-go's order with the spec's changes, on one tree
/// (`runner`, started by its first step, printed once when the run ends:
/// ops decision 39, term-ui decision 14):
///
/// 1. both checkouts opened and their branches read, then
///    [`missing_answers`] with `terminal` (term-ui's `can_ask`) for the
///    checkouts that could be read;
/// 2. host's disk check of `disk_paths`;
/// 3. self-update's steps: replaced, the run ends here with a requested
///    exit 0 and release's [`RERUN`] warning;
/// 4. the branch questions, Saltbox's then Sandbox's ([`branch::resolve`]:
///    `ask`, on the alternate screen in sb, the tree paused over it),
///    each answer a notice under its checkout's step, each followed by
///    its local changes question ([`discard::decide`], decision 40);
///    under `--skip-git` none, both checkouts left as they are;
/// 5. "Updating apt package cache"; each checkout's announcements read;
///    "Updating Saltbox and Sandbox" (both fetches and resets, at once
///    where the host has the cores, then Saltbox's runtime: decision
///    46); "Securing Saltbox configuration permissions"; the tags caches
///    when a commit moved or none are cached; "Checking for new
///    announcements". A checkout that couldn't be opened or read fails
///    the checkouts' step (`error updating <Repo>: ...`), Sandbox's after
///    Saltbox's git phase, as sb-go's `requireDirectory` does;
/// 6. the new announcements: on stdout's terminal tui's viewer (the tree
///    paused over it), every entry that applies marked new or not, else
///    the new ones inline; then their [`migrations`];
/// 7. validate-config's checks (sb-config's `run`), then, when they
///    passed, the completion refresh ([`completions`], a failure a
///    warning);
/// 8. the tree printed, then what moved, on `lines` (ops spec, "What
///    moved, and the closing hint"): each checkout whose pull finished,
///    and the closing hint when the run ended well; after a failure from
///    step 5 on too, without the hint (decision 38).
///
/// # Errors
///
/// A refusal before any step (its text), self-update's failure (`error
/// during self-update: ...`), an unanswered question, or a failed step's
/// quiet `Exit` (its block shown in the tree), the completion refresh's
/// included when Ctrl-C or a signal stops it.
pub async fn run(
    update: &Update<'_>,
    terminal: Result<(), String>,
    mut ask: impl FnMut(&str, &str, &'static str) -> Result<bool, PromptError>,
    confirm: impl FnOnce(&str, &str) -> Result<bool, PromptError>,
    runner: impl FnOnce() -> Runner,
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
    let answer = update.args.branch_flags.answer();
    let (skip_git, discard_local) = (update.args.skip_git, update.args.discard.discard_local);
    // Under `--skip-git` nothing is asked (decision 41).
    let [saltbox_read, sandbox_read] = if skip_git {
        [None, None]
    } else {
        up_front(update.args, terminal.err(), [&saltbox, &sandbox], shutdown).await?
    };
    update.disk.check_disk_space(update.disk_paths)?;
    // One tree for the run (decision 39), started by its first step:
    // self-update's, or apt's, so a run refused at a branch question with
    // no self-update draws no title. std's `LazyCell`
    // (<https://doc.rust-lang.org/std/cell/struct.LazyCell.html#method.force_mut>),
    // as ansible's `LazyRunner`.
    let mut tree = LazyCell::new(runner);
    if let Some(self_update) = update.self_update {
        let outcome =
            sb_release::self_update(update.releases, self_update, LazyCell::force_mut(&mut tree))
                .await
                .context("error during self-update")?;
        if matches!(outcome, Outcome::Updated(_)) {
            // The new sb isn't run in this process (sb-go 1123432): the
            // run ends successfully here, as sb-go's `Shutdown(ctx, 0)`.
            let _ = shutdown.request(0);
            return Ok(());
        }
    }
    // A question on the alternate screen, the tree (when self-update
    // started it) paused over it (decision 39).
    let mut asked =
        |tree: &mut LazyCell<Runner, _>, shown: &str, question: &str, flag: &'static str| {
            if let Some(steps) = LazyCell::get_mut(tree) {
                steps.pause_over();
            }
            ask(shown, question, flag)
        };
    let answers = Answers {
        branch: answer,
        discard_local,
        skip_git,
    };
    let saltbox = plan(
        saltbox,
        saltbox_read,
        answers,
        |shown: &str, question: &str, flag: &'static str| asked(&mut tree, shown, question, flag),
        shutdown,
    )
    .await?;
    let sandbox = plan(
        sandbox,
        sandbox_read,
        answers,
        |shown: &str, question: &str, flag: &'static str| asked(&mut tree, shown, question, flag),
        shutdown,
    )
    .await?;
    // What moved, once the tree is printed, whatever ended the run (ops
    // spec, "What moved, and the closing hint"): the checkouts whose pull
    // finished, the hint only after a finished run (decision 38).
    let mut pulled = Vec::new();
    let result = checkouts_onward(
        update,
        [saltbox, sandbox],
        confirm,
        LazyCell::force_mut(&mut tree),
        shutdown,
        lines,
        &mut pulled,
    )
    .await;
    // The tree, printed once, at the run's end (decision 39).
    drop(tree);
    let block = moved::render(&pulled, result.is_ok(), theme::current());
    let written = if block.is_empty() {
        Ok(())
    } else {
        // `lines` is anstream's stderr in sb: plain when colour is off.
        lines
            .write_all(block.as_bytes())
            .and_then(|()| lines.flush())
            .map_err(|err| exit::output_error(err).into())
    };
    result.and(written)
}

/// The up-front check before any step ([`missing_answers`]), for the
/// checkouts that could be read: each off master, and, without a
/// terminal (`no_terminal`, why not) and `--discard-local`, each whose
/// branch is decided and whose update would discard local changes
/// (decision 40). What it read for each checkout, in `checkouts`' order,
/// so its question in [`plan`] doesn't read it again.
///
/// # Errors
///
/// [`missing_answers`]' refusal.
async fn up_front(
    args: &UpdateArgs,
    no_terminal: Option<String>,
    checkouts: [&anyhow::Result<Opened>; 2],
    shutdown: &Shutdown,
) -> anyhow::Result<[Option<UpFront>; 2]> {
    let answer = args.branch_flags.answer();
    let opened: Vec<&Opened> = checkouts
        .into_iter()
        .filter_map(|opened| opened.as_ref().ok())
        .collect();
    // Off master is what repo's `branch_choice` would ask about.
    let off_master: Vec<(&str, &str)> = opened
        .iter()
        .filter(|opened| branch_choice(&opened.current, DEFAULT_BRANCH, None) == Choice::Ask)
        .map(|opened| (opened.repo.name(), opened.current.as_str()))
        .collect();
    let mut read = [None, None];
    if no_terminal.is_some() && !args.discard.discard_local {
        for (slot, opened) in read.iter_mut().zip(checkouts) {
            let Ok(opened) = opened else { continue };
            let branch = match branch_choice(&opened.current, DEFAULT_BRANCH, answer) {
                Choice::Use(branch) | Choice::Keep(branch) => branch,
                Choice::Ask => continue,
            };
            *slot = discard::read(&opened.repo, &opened.current, &branch, shutdown)
                .await
                .map(|discards| UpFront { branch, discards });
        }
    }
    // A read is there only for a checkout that opened: its name.
    let dirty: Vec<(&str, String)> = read
        .iter()
        .zip(checkouts)
        .filter_map(|(read, opened)| Some((read.as_ref()?, opened.as_ref().ok()?)))
        .filter(|(read, _)| !read.discards.is_empty())
        .map(|(read, opened)| (opened.repo.name(), read.discards.summary()))
        .collect();
    missing_answers(
        no_terminal.as_deref(),
        &off_master,
        answer,
        args.migrations,
        &dirty,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(read)
}

/// What the up-front check read of a checkout: what an update to
/// `branch` would discard.
#[derive(Debug, PartialEq, Eq)]
struct UpFront {
    branch: String,
    discards: Discards,
}

/// update's steps once the questions are answered (`run`'s 5 to 7), each
/// checkout whose pull finished recorded in `pulled`.
///
/// # Errors
///
/// As `run`'s, from its step 5 on.
async fn checkouts_onward(
    update: &Update<'_>,
    [saltbox, sandbox]: [anyhow::Result<(Repo, Planned)>; 2],
    confirm: impl FnOnce(&str, &str) -> Result<bool, PromptError>,
    steps: &mut Runner,
    shutdown: &Shutdown,
    lines: &mut impl Write,
    pulled: &mut Vec<Moved>,
) -> anyhow::Result<()> {
    let skip_git = update.args.skip_git;
    let found = async {
        apt_cache(steps, update.apt).await?;
        // Each checkout's announcements before its pull (sb-go reads both
        // here, after apt); none under `--skip-git`, which compares none.
        let before: Vec<_> = if skip_git {
            Vec::new()
        } else {
            [Checkout::Saltbox, Checkout::Sandbox]
                .into_iter()
                .map(|checkout| {
                    announcements::load(&announcements::own_file(checkout, update.root))
                })
                .collect()
        };
        let mut tags = TagCache::load(update.root);
        let mut cache = Cache {
            tags: &mut tags,
            pulled,
        };
        // Both checkouts' git phases, then Saltbox's runtime (step 7),
        // the permissions (step 8), and only then, nothing having failed,
        // the tag caches (step 9a, decision 46).
        let dues = checkouts_step(steps, [saltbox, sandbox], update, &mut cache).await?;
        secure_step(steps, update.root).await?;
        tag_steps(steps, &mut cache, update.ansible, shutdown, dues).await?;
        if skip_git {
            // The step says so, its one notice where to see them
            // (decision 41): nothing is compared, so nothing is shown or
            // offered.
            let labels = Labels::new("Checking for new announcements", NOT_COMPARED);
            steps
                .step(labels, async |step| {
                    tracing::info!(reason = "--skip-git", "announcements not compared");
                    step.info(SEE_THEM);
                    Ok(())
                })
                .await?;
            return Ok(announcements::Found {
                shown: Vec::new(),
                listed: Vec::new(),
            });
        }
        let before = [Checkout::Saltbox, Checkout::Sandbox]
            .into_iter()
            .zip(before)
            .collect();
        announcements::compare(steps, update.root, before, None).await
    }
    .await;
    let announcements::Found { shown, listed } = found?;
    // After the step that found them (ops spec, "Shown"): on stdout's
    // terminal the viewer, on the alternate screen over the paused tree
    // (decision 39), its every entry that applies, the new ones marked
    // (tui spec, "The announcement viewer", Input); piped, the new ones
    // inline on stdout (decision 24), the tree paused below them, since
    // stderr may be the terminal the pipe's reader shows them on.
    if !shown.is_empty() {
        if update.stdout_terminal {
            steps.pause_over();
            let ending =
                announcements::view(announcements::viewer_entries(listed), shutdown).await?;
            left_viewer(ending, &shown, steps)?;
        } else {
            steps.pause_below();
            announcements::show(&shown)?;
        }
    }
    migrations(update, &shown, confirm, steps, shutdown, lines).await?;
    let validated = sb_config::run::run(steps, update.settings, update.backups).await;
    // Then the completion refresh, under a step of its own, a failure a
    // warning (step 12); sb-go's update returns before it when
    // validation failed.
    let refreshed = if validated.is_ok() {
        completions::refresh(steps, &update.completions).await
    } else {
        Ok(())
    };
    validated.context("error validating Saltbox configuration")?;
    Ok(refreshed?)
}

/// The migrations the new announcements `shown` name (ops spec,
/// "Migrations"; sb-go's `PromptForMigrations` and `ExecuteMigrations`),
/// all or none: `--migrations`, or else [`MIGRATIONS_QUESTION`]
/// (`confirm`, on the alternate screen with their table as its context,
/// `steps` paused over it; default no). Their table goes to stdout too
/// when stdout isn't the tree's terminal. The decision is a notice under
/// the run's newest step (decision 39): `Migrations run: <repo> <tag>,
/// ...`, or the warning `Skipped migrations: ...` (decision 22). Run:
/// `steps` paused below, `Starting migration execution`, then for each
/// `Running migration '<tag>' for <Repo> repository` and ansible's run of
/// that tag in that checkout's playbook, attached, its lines on stdout as
/// `sb install`'s, then `All migrations completed successfully`. sb's
/// lines go to `lines`, a stream that strips colour when it is off
/// (anstream's); the run log gets the decision and each migration.
///
/// # Errors
///
/// The question unanswered; a migration that fails (`failed to execute
/// migration '<tag>' for <Repo> repository (to run by hand: ...): ...`,
/// exit 1, it and the ones after it named as the lines that run them by
/// hand), or is interrupted (the same lines in a warning, then exec's
/// quiet exit, sb ending by the signal); `lines` that can't be written.
async fn migrations(
    update: &Update<'_>,
    shown: &[(Checkout, Vec<Announcement>)],
    confirm: impl FnOnce(&str, &str) -> Result<bool, PromptError>,
    steps: &mut Runner,
    shutdown: &Shutdown,
    lines: &mut impl Write,
) -> anyhow::Result<()> {
    let pending = announcements::migrations(shown);
    if pending.is_empty() {
        return Ok(());
    }
    // On stdout as before when it is a pipe or a file; on its terminal the
    // tree is live, so the table is the question's context only.
    if !update.stdout_terminal {
        steps.pause_below();
        announcements::show_migrations(&pending)?;
    }
    let approved = match update.args.migrations {
        Some(Migrations::Run) => true,
        Some(Migrations::Skip) => false,
        None => {
            steps.pause_over();
            let colour = sb_term_ui::colour_on(sb_term_ui::Stream::Stderr);
            let context = announcements::migrations_table(&pending, colour, None);
            confirm(MIGRATIONS_QUESTION, &context)?
        }
    };
    let named = named(&pending);
    // The run log keeps what was decided and what ran (decision 22), as
    // the runner's notices reach it; these lines aren't a runner's, since
    // the playbooks run attached.
    tracing::info!(
        migrations = %named,
        approved,
        by_flag = update.args.migrations.is_some(),
        "migrations decided"
    );
    if !approved {
        tracing::warn!(migrations = %named, "migrations skipped");
        steps.warn(format!("Skipped migrations: {named}"));
        return Ok(());
    }
    steps.info(format!("Migrations run: {named}"));
    // The playbooks are attached and write on the normal screen: the live
    // tree is drawn again below them.
    steps.pause_below();
    run_pending(&pending, update.root, update.ansible, shutdown, lines).await
}

/// `pending`'s names as the warnings and the run log give them: `<Repo>
/// <tag>`, `, `-joined.
fn named(pending: &[Pending]) -> String {
    pending
        .iter()
        .map(|migration| format!("{} {}", migration.repo.name(), migration.tag))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `text` as one of sb's lines on `lines`, flushed.
///
/// # Errors
///
/// cli-core's `OutputError` when `lines` can't be written.
pub(crate) fn write_line(lines: &mut dyn Write, text: &str) -> anyhow::Result<()> {
    writeln!(lines, "{text}")
        .and_then(|()| lines.flush())
        .map_err(exit::output_error)?;
    Ok(())
}

/// The migrations not run, declined or skipped by the flag: the warning
/// `Skipped migrations: <repo> <tag>, ...` (decision 22), and the run
/// log's record of them.
///
/// # Errors
///
/// `lines` that can't be written.
pub(crate) fn warn_skipped(lines: &mut dyn Write, pending: &[Pending]) -> anyhow::Result<()> {
    let named = named(pending);
    tracing::warn!(migrations = %named, "migrations skipped");
    // sb's `warning:` line, as the binary's own: the theme's warn colour,
    // which `lines` (anstream's stderr in sb) strips when colour is off.
    let style = theme::current().style(theme::Role::Warn);
    write_line(
        lines,
        &format!(
            "{style}warning:{style:#} {}",
            exit::escaped(&format!("Skipped migrations: {named}"))
        ),
    )
}

/// The migrations `pending`, run (ops spec, "Migrations"; sb-go's
/// `ExecuteMigrations`): `Starting migration execution`, then for each
/// `Running migration '<tag>' for <Repo> repository` and ansible's run of
/// that tag in that checkout's playbook under the Saltbox root `root`,
/// attached, its lines on stdout as `sb install`'s, then `All migrations
/// completed successfully`. `update`'s and `announcements preview`'s.
///
/// # Errors
///
/// A migration that fails (`failed to execute migration '<tag>' for
/// <Repo> repository (to run by hand: ...): ...`, exit 1, it and the ones
/// after it named as the lines that run them by hand), or is interrupted
/// (the same lines in a warning, then exec's quiet exit, sb ending by the
/// signal); `lines` that can't be written.
pub(crate) async fn run_pending(
    pending: &[Pending],
    root: &Path,
    ansible: &Ansible,
    shutdown: &Shutdown,
    lines: &mut impl Write,
) -> anyhow::Result<()> {
    let write = write_line;
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
        // A failure or an interrupt names this migration and those after
        // it, which the next update won't offer again (the announcements
        // are no longer new): each as the line that runs it by hand
        // (difference 43).
        let left = || by_hand(&pending[at..]);
        // In parentheses, so the cause shown after it isn't read as the
        // last line's.
        let context = || {
            format!(
                "failed to execute migration '{tag}' for {name} repository (to run by hand: {})",
                left()
            )
        };
        // ansible's lines on stdout, as `sb install`'s.
        let ran = ansible
            .run(
                &migration.repo.playbooks(root),
                Tags::Given(&tags),
                &RunArgs::default(),
                shutdown,
                &mut sb_cli_core::tail::Tracked::stdout(io::stdout()),
            )
            .await
            .with_context(context)?;
        // An interrupted run (exec's `attached`: sb's SIGTERM or hangup, a
        // Ctrl-C before the child started or one it died of): the
        // warning, then exec's quiet exit (its `pass_through`), cli-core
        // ending sb by the signal. Ansible itself takes Ctrl-C and exits
        // 99 (ansible spec, assumption 5): the failure below, its list
        // the same.
        if ran.outcome.ending == Ending::Interrupted {
            warn_unrun(lines, &left())?;
            return Err(Exit(1).into());
        }
        // It ran: Ansible's output above says why, so no log pointer
        // (the maintainer, 2026-11-18). A playbook that couldn't start
        // keeps it (the `?` on `run` above).
        let _ran = ran
            .check()
            .map_err(exit::no_pointer)
            .with_context(context)?;
        tracing::info!(repo = name, tag = %tag, "migration completed");
    }
    write(lines, "All migrations completed successfully")
}

/// After the viewer: closed (`q`, Esc), the run goes on. Left by Ctrl-C
/// or a signal, the run ends there, its pending migrations named first
/// as an interrupted migration names them (`Migrations interrupted; to
/// run by hand: ...`, decision 22 and difference 43), since the next
/// update won't offer them again, in a warning notice of `steps`' tree:
/// Ctrl-C with 130, as the viewer's other callers, a signal with the
/// quiet exit cli-core ends by.
///
/// # Errors
///
/// The ending.
fn left_viewer(
    ending: sb_tui::Ending,
    shown: &[(Checkout, Vec<Announcement>)],
    steps: &mut Runner,
) -> anyhow::Result<()> {
    if ending == sb_tui::Ending::Quit {
        return Ok(());
    }
    let pending = announcements::migrations(shown);
    if !pending.is_empty() {
        // A notice in the tree, printed at the run's end (decision 39).
        let unrun = by_hand(&pending);
        tracing::warn!(migrations = %unrun, "migrations interrupted");
        steps.warn(format!("Migrations interrupted; to run by hand: {unrun}"));
    }
    let code = if ending == sb_tui::Ending::Interrupted {
        130
    } else {
        1
    };
    Err(Exit(code).into())
}

/// The warning naming the migrations not run, `unrun` (the `sb install`
/// lines), and the run log's record of them.
///
/// # Errors
///
/// `lines` that can't be written.
fn warn_unrun(lines: &mut dyn Write, unrun: &str) -> anyhow::Result<()> {
    tracing::warn!(migrations = %unrun, "migrations interrupted");
    let style = theme::current().style(theme::Role::Warn);
    writeln!(
        lines,
        "{style}warning:{style:#} {}",
        exit::escaped(&format!("Migrations interrupted; to run by hand: {unrun}"))
    )
    .and_then(|()| lines.flush())
    .map_err(exit::output_error)?;
    Ok(())
}

/// The migrations `left` as the `sb install` lines that run them by
/// hand, `, `-joined: Saltbox's tag as it is, Sandbox's with its
/// `sandbox-` prefix, as `sb install` routes tags (ansible spec).
pub(crate) fn by_hand(left: &[Pending]) -> String {
    let lines: Vec<String> = left
        .iter()
        .map(|migration| match migration.repo {
            Checkout::Saltbox => format!("sb install {}", migration.tag),
            Checkout::Sandbox => format!("sb install sandbox-{}", migration.tag),
        })
        .collect();
    lines.join(", ")
}

/// The flags that answer update's questions.
#[derive(Debug, Clone, Copy)]
struct Answers {
    branch: Option<Answer>,
    discard_local: bool,
    skip_git: bool,
}

/// What a checkout's step does with its git, decided before any step.
#[derive(Debug)]
struct Planned {
    /// The notices for its step: the branch's, then the local changes';
    /// for a checkout left as it is, only the latter.
    notices: Vec<String>,
    /// Its git steps.
    git: Git,
}

/// A checkout's git steps.
#[derive(Debug)]
enum Git {
    /// Fetched and reset to this branch.
    Update(String),
    /// Left as it is, and why, for the run log (`--skip-git`,
    /// `declined`).
    Left(&'static str),
}

/// The questions for an opened checkout: its branch ([`branch::resolve`])
/// and then its local changes ([`discard::decide`], decision 40), both
/// through `ask`; under `--skip-git` neither, the checkout left as it
/// is. The outer error ends the run before any step (an unanswered
/// question); the inner one is the checkout's that couldn't be opened or
/// read, kept for its own step. What the up-front check read (`read`)
/// is used for the branch it was read for, not read again.
///
/// # Errors
///
/// A question unanswered, or a refused branch name (`error updating
/// <Repo>: ...`).
async fn plan(
    opened: anyhow::Result<Opened>,
    read: Option<UpFront>,
    answers: Answers,
    mut ask: impl FnMut(&str, &str, &'static str) -> Result<bool, PromptError>,
    shutdown: &Shutdown,
) -> anyhow::Result<anyhow::Result<(Repo, Planned)>> {
    let opened = match opened {
        Ok(opened) => opened,
        Err(err) => return Ok(Err(err)),
    };
    if answers.skip_git {
        let planned = Planned {
            notices: Vec::new(),
            git: Git::Left("--skip-git"),
        };
        return Ok(Ok((opened.repo, planned)));
    }
    let name = opened.repo.name().to_owned();
    let context = || format!("error updating {name}");
    let Resolved {
        branch,
        mut notices,
    } = branch::resolve(
        &opened.repo,
        &opened.current,
        DEFAULT_BRANCH,
        answers.branch,
        |shown: &str, question: &str| ask(shown, question, BRANCH_FLAGS),
        shutdown,
    )
    .await
    .with_context(context)?;
    let doing = format!("updating to '{branch}'");
    let decided = match read {
        Some(read) if read.branch == branch => discard::decide_on(
            &opened.repo,
            &read.discards,
            answers.discard_local,
            purpose(&doing),
            ask,
        ),
        _ => {
            discard::decide(
                &opened.repo,
                &opened.current,
                &branch,
                answers.discard_local,
                purpose(&doing),
                ask,
                shutdown,
            )
            .await
        }
    };
    let Decision { update, notice } = match decided {
        Ok(decision) => decision,
        // git couldn't read the checkout: its own step fails on it.
        Err(err) if err.downcast_ref::<sb_repo::RepoError>().is_some() => {
            return Ok(Err(err));
        }
        Err(err) => return Err(err.context(context())),
    };
    // Left as it is, the branch's notice (a reset, the current branch
    // updated) would say what didn't happen: the answer alone is noted.
    let planned = if update {
        notices.extend(notice);
        Planned {
            notices,
            git: Git::Update(branch),
        }
    } else {
        Planned {
            notices: notice.into_iter().collect(),
            git: Git::Left("declined"),
        }
    };
    Ok(Ok((opened.repo, planned)))
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

/// The tags cache the tag steps rebuild, and what the checkouts' pulls
/// moved.
struct Cache<'a> {
    tags: &'a mut TagCache,
    pulled: &'a mut Vec<Moved>,
}

/// A checkout's tags rebuild, due once both checkouts' git steps have
/// ended (step 9a, decision 46).
#[derive(Debug)]
struct Due {
    checkout: Checkout,
    playbooks: sb_ansible::Repo,
    /// The HEAD its git phase read after the pull, which the entry
    /// records.
    head: String,
    /// Left as it is: rebuilt always, nothing compared (decision 41).
    left: bool,
    /// The old entry, when valid for the commit before the pull
    /// (decision 37), read before any rebuild writes over it.
    before: Option<Vec<String>>,
    /// Its checkout's place in `pulled`.
    at: usize,
}

/// Step 9a (decision 46): "Updating Saltbox tags cache", then "Updating
/// Sandbox tags cache", each top-level step only when `dues` has it. With
/// both due on 2 or more physical cores (ansible's `listings_at_once`),
/// Sandbox's listing starts with Saltbox's (it stops with `shutdown`'s
/// token, the run's) and runs alongside Saltbox's step; Sandbox's step
/// waits for what is left of it. Each step runs whatever the other's
/// result, and what a rebuild lists is compared with the old entry.
///
/// # Errors
///
/// The first failed step's quiet `Exit`, Saltbox's before Sandbox's.
async fn tag_steps(
    runner: &mut Runner,
    cache: &mut Cache<'_>,
    ansible: &Ansible,
    shutdown: &Shutdown,
    [saltbox, sandbox]: [Option<Due>; 2],
) -> Result<(), Exit> {
    let mut started = match (&saltbox, &sandbox) {
        (Some(_), Some(due)) if ansible.listings_at_once() => Some(StartedListing::start(
            ansible,
            &due.playbooks,
            due.head.clone(),
            &shutdown.token(),
        )),
        _ => None,
    };
    let mut first = Ok(());
    if let Some(due) = &saltbox {
        let done = tag_step(runner, cache.tags, ansible, due, None, started.as_mut()).await;
        first = first.and(compared(cache, due, done));
    }
    if let Some(due) = &sandbox {
        let done = tag_step(runner, cache.tags, ansible, due, started.take(), None).await;
        first = first.and(compared(cache, due, done));
    }
    first
}

/// One tag step, "Updating <Repo> tags cache": `started` waited for
/// (Sandbox's, started at once), else `due`'s listing started under the
/// step; `beside` (the other listing) runs alongside it. A step that
/// doesn't start (after Ctrl-C) stops `started` and waits for it.
///
/// # Errors
///
/// The step's quiet `Exit`, its block shown.
async fn tag_step(
    runner: &mut Runner,
    tags: &mut TagCache,
    ansible: &Ansible,
    due: &Due,
    started: Option<StartedListing<'_>>,
    beside: Option<&mut StartedListing<'_>>,
) -> Result<Vec<String>, Exit> {
    let label = sb_ansible::cache_label(due.playbooks.kind());
    let mut waiting = started;
    let done = runner
        .step(Labels::new(label.clone(), label), async |step| {
            let token = step.token();
            let listing = waiting.take().unwrap_or_else(|| {
                StartedListing::start(ansible, &due.playbooks, due.head.clone(), &token)
            });
            let recorded = tags.record_started(listing);
            match beside {
                Some(other) => other.alongside(recorded).await,
                None => recorded.await,
            }
            .context("error running and caching ansible tags")
        })
        .await;
    if let Some(started) = waiting {
        started.stop().await;
    }
    done
}

/// A tag step's result: the new tags compared with `due`'s old entry, for
/// what moved (decision 37), when it had one and wasn't left as it is.
///
/// # Errors
///
/// The step's quiet `Exit`.
fn compared(cache: &mut Cache<'_>, due: &Due, done: Result<Vec<String>, Exit>) -> Result<(), Exit> {
    let after = done?;
    if due.left {
        return Ok(());
    }
    let name = due.checkout.name();
    if let Some(before) = &due.before {
        let diff = TagDiff::between(due.checkout, before, &after);
        tracing::info!(
            repo = name,
            added = %diff.added.join(", "),
            removed = %diff.removed.join(", "),
            "tags compared"
        );
        cache.pulled[due.at].tags = Some(diff);
    } else {
        tracing::info!(
            repo = name,
            reason = "no valid cache entry for the old commit",
            "tags not compared"
        );
    }
    Ok(())
}

/// Step 7 (decision 46): "Updating Saltbox and Sandbox" (success
/// "Saltbox and Sandbox updated"): both checkouts' notices (Saltbox's
/// first), the Saltbox user from the Saltbox root's accounts.yml, each
/// checkout's git phase ([`git_phases`]: fetched and reset to its
/// resolved branch and owned by that user, at once where the host has
/// the cores, each pull recorded in `cache.pulled` and the run log), then
/// python-env's `reconcile_runtime` for Saltbox (deadsnakes, venv, fact,
/// as `update-venv`). What it hands on is each checkout's tags rebuild,
/// when the commit moved or the cache has no tags for the checkout, due
/// for step 9a ([`tag_steps`]), with the old entry when valid for the
/// HEAD before the pull (ansible's `fresh`), read now, before the rebuild
/// writes over it (ops spec, "What moved, and the closing hint"). A
/// checkout that couldn't be opened fails here with its error (sb-go's
/// `error updating <Repo>: inspect required directory ...`): Saltbox's
/// before anything runs, Sandbox's after Saltbox's git phase.
async fn checkouts_step(
    runner: &mut Runner,
    [saltbox, sandbox]: [anyhow::Result<(Repo, Planned)>; 2],
    update: &Update<'_>,
    cache: &mut Cache<'_>,
) -> Result<[Option<Due>; 2], Exit> {
    let labels = Labels::new(
        "Updating Saltbox and Sandbox",
        "Saltbox and Sandbox updated",
    )
    .failure("Saltbox and Sandbox update");
    runner
        .step(labels, async |step: &mut Step| {
            let saltbox = saltbox.context("error updating Saltbox")?;
            let planned = std::iter::once(&saltbox).chain(sandbox.as_ref().ok());
            for notice in planned.flat_map(|(_, planned)| &planned.notices) {
                step.info(notice);
            }
            let user =
                sb_config::user::saltbox_user(update.root).context("error getting saltbox user")?;
            let phases = git_phases(step, saltbox, sandbox, update, &user, cache).await?;
            reconcile_runtime(step, update.python, Some(&user)).await?;
            Ok(phases.map(|phase| due(cache, update.root, phase)))
        })
        .await
}

/// What a checkout's git phase left: its HEAD before and after, whether
/// it was left as it is, and its place in `pulled`.
#[derive(Debug)]
struct Phase {
    checkout: Checkout,
    old: String,
    new: String,
    left: bool,
    at: usize,
}

/// Both checkouts' git phases under `step`, Saltbox's drawn first. With
/// both to update on 2 or more physical cores ([`Update::git_at_once`],
/// else host's `cores` rule) they run at once as term-ui's `group_all`
/// (each to its end: a failure doesn't stop the other's reset half-way),
/// both pulls recorded that finished, the first failure, Saltbox's
/// first, returned; otherwise in turn, a failure stopping before the
/// next. A checkout left as it is reads its branch and changes instead.
///
/// # Errors
///
/// Sandbox's checkout that couldn't be opened (after Saltbox's git
/// phase), git's reads, or a failed git phase's quiet `Exit`.
async fn git_phases(
    step: &mut Step,
    saltbox: (Repo, Planned),
    sandbox: anyhow::Result<(Repo, Planned)>,
    update: &Update<'_>,
    user: &str,
    cache: &mut Cache<'_>,
) -> anyhow::Result<[Phase; 2]> {
    if let Ok(sandbox) = &sandbox
        && let (Git::Update(saltbox_branch), Git::Update(sandbox_branch)) =
            (&saltbox.1.git, &sandbox.1.git)
        && git_at_once(update)
    {
        let olds = [
            old_head(&saltbox.0, step).await?,
            old_head(&sandbox.0, step).await?,
        ];
        // Each sibling reads its HEAD after its own pull, so a pull that
        // finished is known (and recorded below) even when the other is
        // interrupted or fails afterwards (decision 38), as in turn.
        let mut group = step.group_all::<String>();
        for (repo, branch) in [(&saltbox.0, saltbox_branch), (&sandbox.0, sandbox_branch)] {
            let (repo, branch, user) = (repo.clone(), branch.clone(), user.to_owned());
            group.spawn(
                branch::repository_labels(repo.name(), &branch),
                async move |mut step| {
                    repo.fetch_and_reset(&branch, &user, &mut step).await?;
                    repo.head(&step.token())
                        .await
                        .context("error getting new commit hash")
                },
            );
        }
        let ended = group.join().await;
        // Every finished pull recorded before the first failure returns
        // (why not `collect::<Result<_, _>>`: it would stop at Saltbox's
        // failure, before Sandbox's finished pull).
        let checkouts = [
            (Checkout::Saltbox, &saltbox.0),
            (Checkout::Sandbox, &sandbox.0),
        ];
        let mut phases = Vec::new();
        let mut first = None;
        for ((checkout, _), (old, ended)) in checkouts.into_iter().zip(olds.into_iter().zip(ended))
        {
            match ended {
                Ok(new) => phases.push(pulled(cache, checkout, old, new)),
                Err(exit) => {
                    first.get_or_insert(exit);
                }
            }
        }
        if let Some(exit) = first {
            return Err(exit.into());
        }
        let [saltbox, sandbox]: [Phase; 2] = phases
            .try_into()
            .expect("group_all gives one result for each of its two siblings");
        return Ok([saltbox, sandbox]);
    }
    let saltbox = phase(step, Checkout::Saltbox, saltbox, user, cache).await?;
    let sandbox = sandbox.context("error updating Sandbox")?;
    let sandbox = phase(step, Checkout::Sandbox, sandbox, user, cache).await?;
    Ok([saltbox, sandbox])
}

/// Whether both checkouts' git phases run at once: [`Update::git_at_once`],
/// or one per physical core sb may run on (host's `cores`, the rule the
/// tag listings and python-env's probes take), noted in the run log.
fn git_at_once(update: &Update<'_>) -> bool {
    let cores = update
        .git_at_once
        .unwrap_or_else(sb_host::cores::children_at_once);
    let at_once = cores.get() >= 2;
    tracing::info!(cores, at_once, "git phases at once");
    at_once
}

/// One checkout's git phase in turn under `step`: its HEAD before, then
/// "Updating <Repo> repository" and its pull recorded, or, left as it is,
/// its branch and changes read.
///
/// # Errors
///
/// git's reads, or the repository step's quiet `Exit`.
async fn phase(
    step: &mut Step,
    checkout: Checkout,
    (repo, planned): (Repo, Planned),
    user: &str,
    cache: &mut Cache<'_>,
) -> anyhow::Result<Phase> {
    let old = old_head(&repo, step).await?;
    match &planned.git {
        Git::Update(branch) => {
            branch::update(step, &repo, branch, user).await?;
            let new = repo
                .head(&step.token())
                .await
                .context("error getting new commit hash")?;
            Ok(pulled(cache, checkout, old, new))
        }
        Git::Left(reason) => {
            let left = left_as_is(&repo, reason, &old, step).await?;
            let at = cache.pulled.len();
            cache.pulled.push(Moved {
                checkout,
                old: old.clone(),
                new: old.clone(),
                tags: None,
                left: Some(left),
            });
            Ok(Phase {
                checkout,
                new: old.clone(),
                old,
                left: true,
                at,
            })
        }
    }
}

/// A checkout's HEAD before its git phase.
///
/// # Errors
///
/// The HEAD's read, as `error getting old commit hash`.
async fn old_head(repo: &Repo, step: &Step) -> anyhow::Result<String> {
    repo.head(&step.token())
        .await
        .context("error getting old commit hash")
}

/// A finished pull, its HEAD after it read right after the fetch and
/// reset (HEAD doesn't move in the runtime, and what moved is then known
/// if the runtime fails: decision 38), recorded in `cache.pulled` and the
/// run log.
fn pulled(cache: &mut Cache<'_>, checkout: Checkout, old: String, new: String) -> Phase {
    tracing::info!(repo = checkout.name(), old = %old, new = %new, "checkout pulled");
    let at = cache.pulled.len();
    cache.pulled.push(Moved {
        checkout,
        old: old.clone(),
        new: new.clone(),
        tags: None,
        left: None,
    });
    Phase {
        checkout,
        old,
        new,
        left: false,
        at,
    }
}

/// A checkout's tags rebuild after its git phase, if one is due: sb-go's
/// rule for update (ops spec, step 9a), rebuilt when the commit moved or
/// no tags are cached. Why not `TagCache::tags`, which also rebuilds an
/// entry cached at another commit: that isn't sb-go's update rule. A
/// checkout left as it is is rebuilt always (decision 41): its HEAD can't
/// move, and its roles may have changed by hand; no tags are compared
/// then. The old entry only when valid for the commit before the pull
/// (decision 37), read before the rebuild writes over it.
fn due(cache: &Cache<'_>, root: &Path, phase: Phase) -> Option<Due> {
    let playbooks = phase.checkout.playbooks(root);
    let cached = cache
        .tags
        .entry(&playbooks)
        .is_some_and(|entry| !entry.tags.is_empty());
    if !phase.left && phase.old == phase.new && cached {
        return None;
    }
    let before = (!phase.left)
        .then(|| cache.tags.fresh(&playbooks, &phase.old))
        .flatten();
    Some(Due {
        checkout: phase.checkout,
        playbooks,
        head: phase.new,
        left: phase.left,
        before,
        at: phase.at,
    })
}

/// A checkout left as it is, for what moved (decisions 40 and 41): its
/// branch and working-tree changes, read under `step`, and the run log's
/// `git skipped` with `reason` and its HEAD, `old`.
///
/// # Errors
///
/// git's reads, as `error reading <Repo>'s branch` or `... local
/// changes`.
async fn left_as_is(repo: &Repo, reason: &str, old: &str, step: &Step) -> anyhow::Result<Left> {
    let (name, token) = (repo.name(), &step.token());
    let branch = repo
        .current_branch(token)
        .await
        .with_context(|| format!("error reading {name}'s branch"))?;
    let changes = repo
        .local_changes(token)
        .await
        .with_context(|| format!("error reading {name}'s local changes"))?
        .count();
    tracing::info!(
        repo = name,
        reason,
        head = %old,
        branch = %branch,
        changes,
        "git skipped"
    );
    Ok(Left { branch, changes })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// git in `dir` on no configuration but its own (as the `testing`
    /// module's git), its stdout trimmed.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=sb", "-c", "user.email=sb"])
            .args(["-c", "init.defaultBranch=master"])
            .args(["-c", "safe.directory=*"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("LC_ALL", "C")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A clean clone of a one-commit `master`, tracking its remote.
    fn clean_checkout(dir: &Path) -> PathBuf {
        git(dir, &["init", "--quiet", "--bare", "remote.git"]);
        let remote = format!("file://{}", dir.join("remote.git").display());
        git(dir, &["clone", "--quiet", &remote, "checkout"]);
        let checkout = dir.join("checkout");
        std::fs::write(checkout.join("file"), "x").expect("file");
        git(&checkout, &["add", "."]);
        git(&checkout, &["commit", "--quiet", "-m", "one"]);
        git(&checkout, &["push", "--quiet", "origin", "master"]);
        checkout
    }

    /// `plan` for the clean checkout at `path` on master, with
    /// `--discard-local` and what the up-front check read (`read`): the
    /// notices planned.
    async fn planned(path: &Path, read: Option<UpFront>) -> Vec<String> {
        let opened = Opened {
            repo: Repo::open("Sandbox", path).expect("open"),
            current: "master".to_owned(),
        };
        let answers = Answers {
            branch: None,
            discard_local: true,
            skip_git: false,
        };
        let never =
            |_: &str, _: &str, _: &'static str| -> Result<bool, PromptError> { panic!("asked") };
        let (_, planned) = plan(Ok(opened), read, answers, never, &Shutdown::new())
            .await
            .expect("planned")
            .expect("read");
        planned.notices
    }

    /// What the up-front check would have read for `branch`: one
    /// untracked file, which the clean checkout doesn't have.
    fn one_untracked(branch: &str) -> UpFront {
        UpFront {
            branch: branch.to_owned(),
            discards: Discards {
                branch: branch.to_owned(),
                changes: sb_repo::Changes {
                    tracked: Vec::new(),
                    untracked: vec!["notes.txt".to_owned()],
                },
                ..Discards::default()
            },
        }
    }

    /// update's flags from `args`, as clap parses them.
    fn flags(args: &[&str]) -> UpdateArgs {
        use clap::Parser as _;
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            args: UpdateArgs,
        }
        Cli::try_parse_from([&["sb", "--migrations", "skip"], args].concat())
            .expect("parses")
            .args
    }

    #[tokio::test]
    async fn without_a_terminal_the_up_front_check_hands_on_what_it_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let checkout = clean_checkout(dir.path());
        let opened = || -> anyhow::Result<Opened> {
            Ok(Opened {
                repo: Repo::open("Saltbox", &checkout)?,
                current: "master".to_owned(),
            })
        };
        let (saltbox, missing) = (opened(), Err(anyhow::anyhow!("not opened")));
        let shutdown = Shutdown::new();
        let clean = UpFront {
            branch: "master".to_owned(),
            discards: Discards {
                branch: "master".to_owned(),
                ..Discards::default()
            },
        };
        let no_terminal = || Some("no terminal".to_owned());
        let read = up_front(&flags(&[]), no_terminal(), [&saltbox, &missing], &shutdown)
            .await
            .expect("nothing to refuse");
        assert_eq!(read, [Some(clean), None]);
        // On a terminal, or with `--discard-local`, nothing is read.
        for (args, terminal) in [(&[][..], None), (&["--discard-local"][..], no_terminal())] {
            let read = up_front(&flags(args), terminal, [&saltbox, &missing], &shutdown)
                .await
                .expect("nothing to refuse");
            assert_eq!(read, [None, None]);
        }
    }

    #[tokio::test]
    async fn the_up_front_read_is_used_for_its_branch_not_read_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let checkout = clean_checkout(dir.path());
        // Used: its untracked file is the notice, though the checkout is
        // clean.
        assert_eq!(
            planned(&checkout, Some(one_untracked("master"))).await,
            ["Sandbox: Discarding 1 untracked file (--discard-local)"]
        );
        // Read for another branch, or not read: read again, clean.
        assert_eq!(
            planned(&checkout, Some(one_untracked("develop"))).await,
            Vec::<String>::new()
        );
        assert_eq!(planned(&checkout, None).await, Vec::<String>::new());
    }

    fn pending(repo: Checkout, tag: &str) -> Pending {
        Pending {
            repo,
            tag: tag.to_owned(),
            date: "2026-11-17".to_owned(),
        }
    }

    /// Each migration as the `sb install` line that runs it: routed back
    /// through `sb install`'s own rule (sb-ansible's `Routed::route`),
    /// each line lands in its checkout with its tag.
    #[test]
    fn migrations_are_the_lines_that_run_them() {
        assert_eq!(by_hand(&[]), "");
        let left = [
            pending(Checkout::Saltbox, "migrate-a"),
            pending(Checkout::Sandbox, "migrate-c"),
        ];
        let text = by_hand(&left);
        let tags: Vec<String> = text
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
