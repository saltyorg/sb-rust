//! `sb announcements preview` (ops spec, "`sb announcements preview`";
//! decision 34): what the next `sb update` would show, for developers.
//! "Before" is each checkout's `announcements.yml` at a git ref (sb-repo's
//! `file_at`), "after" its working tree's; from there it is `update`'s own
//! code: its comparison step, its viewer or inline form, its migration
//! table and question, its migration runner. The migrations are a dry run
//! unless `--run-migrations`.

use std::io::Write;
use std::path::Path;

use anyhow::Context as _;
use sb_ansible::Ansible;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::{self, Exit};
use sb_repo::{Repo, RepoError};
use sb_term_ui::{Labels, PromptError, Runner};

use super::{Announcement, Before, FILE, LoadError, Pending};
use crate::Checkout;
use crate::update::{MIGRATIONS_QUESTION, by_hand, run_pending, warn_skipped, write_line};

/// The line after the would-run lines of a dry run.
pub const DRY_RUN: &str = "Dry run: nothing was run; --run-migrations runs them.";

/// What a preview compares and does.
#[derive(Debug)]
pub struct Preview<'a> {
    /// The Saltbox root ([`Checkout::path`]).
    pub root: &'a Path,
    /// The git ref "before" is read at (`--since`, `HEAD` by default).
    pub since: &'a str,
    /// The one checkout to preview (`--repo`), else both.
    pub repo: Option<Checkout>,
    /// `--run-migrations`: run the migrations when approved, not a dry run.
    pub run_migrations: bool,
    /// `ansible-playbook`, for `--run-migrations`.
    pub ansible: &'a Ansible,
    /// Whether stdout is a terminal: the viewer then, else inline.
    pub stdout_terminal: bool,
}

/// `sb announcements preview`:
///
/// 1. one runner step per checkout, `Reading <Repo> announcements at
///    <ref>`: the file at the ref (none: every entry new); without
///    `--repo` a checkout that isn't there is skipped;
/// 2. `update`'s step "Checking for new announcements", with a line per
///    checkout counting its new ones since the ref;
/// 3. the new ones shown as `update` shows them (the viewer on a
///    terminal, the new ones marked; inline piped);
/// 4. their migrations: `update`'s table, then its question when one can
///    be asked (`can_ask`, term-ui's; `confirm` asks it); without a
///    terminal no question, as yes. Yes: `Would run: sb install <tag>`
///    for each and [`DRY_RUN`], or with `--run-migrations` `update`'s
///    runner; no: `update`'s skipped warning.
///
/// # Errors
///
/// A checkout that can't be read at the ref (`error reading <Repo>
/// announcements at '<ref>': ...`, exit 1, in its step), the viewer's
/// I/O error or `Exit(130)` after Ctrl-C on it, the question unanswered,
/// `update`'s migration errors, or a failed write.
pub async fn preview(
    preview: &Preview<'_>,
    can_ask: Result<(), String>,
    confirm: impl FnOnce(&str) -> Result<bool, PromptError>,
    mut runner: impl FnMut() -> Runner,
    shutdown: &Shutdown,
    lines: &mut impl Write,
) -> anyhow::Result<()> {
    let checkouts: Vec<Checkout> = match preview.repo {
        Some(repo) => vec![repo],
        None => vec![Checkout::Saltbox, Checkout::Sandbox],
    };
    let mut steps = runner();
    let found = async {
        let mut before = Vec::with_capacity(checkouts.len());
        for checkout in checkouts {
            let path = checkout.path(preview.root);
            // A server without Sandbox: skipped, as `update` and
            // `announcements` skip it, unless `--repo` asks for it.
            if preview.repo.is_none() && !path.exists() {
                tracing::info!(repo = checkout.name(), path = %path.display(), "no checkout; skipped");
                continue;
            }
            let read = read_step(&mut steps, checkout, &path, preview.since).await?;
            before.push((checkout, read));
        }
        super::compare(&mut steps, preview.root, before, Some(preview.since)).await
    }
    .await;
    steps.finish();
    let super::Found { shown, listed } = found?;
    if !shown.is_empty() {
        if preview.stdout_terminal {
            let ending = super::view(super::viewer_entries(listed), shutdown).await?;
            // Nothing was going to run unasked, so no migration is named:
            // the preview can be had again.
            match ending {
                sb_tui::Ending::Quit => {}
                sb_tui::Ending::Interrupted => return Err(Exit(130).into()),
                sb_tui::Ending::Cancelled => return Err(Exit(1).into()),
            }
        } else {
            super::show(&shown)?;
        }
    }
    migrations(preview, &shown, can_ask, confirm, shutdown, lines).await
}

/// "Reading <Repo> announcements at <ref>" (success "Read <Repo>
/// announcements at <ref>"): the checkout opened and its file read at
/// the ref, then loaded by the loader. A file that isn't text sb reads,
/// or doesn't load, is the [`LoadError`] `update`'s step warns about; a
/// ref git doesn't know, or a checkout that can't be opened, fails the
/// step.
async fn read_step(
    runner: &mut Runner,
    checkout: Checkout,
    path: &Path,
    since: &str,
) -> Result<Before, Exit> {
    let name = checkout.name();
    let shown = exit::escaped(since);
    let labels = Labels::new(
        format!("Reading {name} announcements at {shown}"),
        format!("Read {name} announcements at {shown}"),
    );
    runner
        .step(labels, async |step| {
            let context = || format!("error reading {name} announcements at '{shown}'");
            let repo = Repo::open(name, path).with_context(context)?;
            let read = match repo.file_at(since, FILE, &step.token()).await {
                Ok(text) => text,
                Err(RepoError::Blob(err)) => return Ok(Err(LoadError::Read(err))),
                Err(err) => return Err(anyhow::Error::from(err).context(context())),
            };
            tracing::info!(
                repo = name,
                since = %shown,
                file = if read.is_some() { "loaded" } else { "missing" },
                "announcements read at ref"
            );
            let Some(text) = read else {
                step.info(format!(
                    "{name} has no {FILE} at {shown}: every entry is new"
                ));
                return Ok(Ok(None));
            };
            let label = format!("{since}:{FILE}");
            Ok(super::parse_file(&label, &text)
                .map(Some)
                .map_err(|issues| LoadError::Parse(super::joined(&issues))))
        })
        .await
}

/// The migrations `shown` names: `update`'s table, its question (or, with
/// no terminal, none, as yes), then the would-run lines and [`DRY_RUN`],
/// or `update`'s runner with `--run-migrations`; declined, `update`'s
/// warning. The run log records what was decided, as `update`'s.
async fn migrations(
    preview: &Preview<'_>,
    shown: &[(Checkout, Vec<Announcement>)],
    can_ask: Result<(), String>,
    confirm: impl FnOnce(&str) -> Result<bool, PromptError>,
    shutdown: &Shutdown,
    lines: &mut impl Write,
) -> anyhow::Result<()> {
    let pending: Vec<Pending> = super::migrations(shown);
    if pending.is_empty() {
        return Ok(());
    }
    super::show_migrations(&pending)?;
    let asked = can_ask.is_ok();
    let approved = if asked {
        confirm(MIGRATIONS_QUESTION)?
    } else {
        true
    };
    tracing::info!(
        migrations = %by_hand(&pending),
        approved,
        asked,
        run = preview.run_migrations,
        "preview migrations decided"
    );
    if !approved {
        return warn_skipped(lines, &pending);
    }
    if preview.run_migrations {
        return run_pending(&pending, preview.root, preview.ansible, shutdown, lines).await;
    }
    for migration in &pending {
        let line = by_hand(std::slice::from_ref(migration));
        write_line(lines, &exit::escaped(&format!("Would run: {line}")))?;
    }
    write_line(lines, DRY_RUN)
}
