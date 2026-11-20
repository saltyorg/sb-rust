//! A checkout's local changes before its update (ops spec, "Local
//! changes, `--discard-local` and `--skip-git`"; decisions 40 to 42):
//! repo's `discards` for the branch it is updated to, then, when there is
//! anything, `--discard-local`'s answer, the question, or a refusal
//! without a terminal. `update`'s, `setup`'s and the branch commands'.

use std::io::Write;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit;
use sb_cli_core::theme;
use sb_repo::{Discards, Repo, RepoError};
use sb_term_ui::PromptError;

/// The names the question lists at most.
const NAMES: usize = 5;

/// The width a listed name is cut to, after its two-space indent: 80
/// columns, as what moved is laid out.
const NAME_WIDTH: usize = 78;

/// `--discard-local`: flattened into each command's arguments
/// (`#[command(flatten)]`, <https://docs.rs/clap/4.6.7/clap/_derive/index.html>).
#[derive(clap::Args, Debug, Clone, Copy, Default)]
pub struct DiscardFlag {
    /// Discard local commits, changes and untracked files in the checkouts without asking
    #[arg(long, global = true)]
    pub discard_local: bool,
}

/// What a command asks about the checkout and how a refusal reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Purpose<'a> {
    /// What the command does to the checkout, after `<Repo>: `: `updating
    /// to 'master'`.
    pub doing: &'a str,
    /// The question's end: `update`, `switch to 'develop'`.
    pub then: &'a str,
    /// What declining means: `the rest of the update goes on`.
    pub declined: &'a str,
    /// The work named in a refusal: `update`, `switch`.
    pub work: &'a str,
    /// The flags a refusal names: `--discard-local or --skip-git`.
    pub flags: &'static str,
}

/// The decision for one checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision {
    /// Whether the checkout's git steps run (false: left as it is).
    pub update: bool,
    /// The notice for the checkout's step, when there was anything to
    /// discard.
    pub notice: Option<String>,
}

/// The decision for `repo`, on `current`, updated to `branch`
/// (decision 40): repo's `discards`; nothing to discard, the update goes
/// on silently; `discard_local`, it goes on with the notice
/// `<Repo>: Discarding <summary> (--discard-local)`; else `ask` (the
/// question on the alternate screen in sb; its context [`context`], its
/// flag `purpose.flags`): yes, the notice
/// `<Repo>: Discarding <summary> (your answer)`; no, the checkout left
/// as it is, the notice
/// `<Repo>: Left as is, keeping <summary> (your answer)`. The run log
/// gets the counts and the decision.
///
/// # Errors
///
/// git's reads ([`RepoError`]); no terminal: the refusal naming the
/// counts and `purpose.flags`; the question's other errors (Ctrl-C).
pub(crate) async fn decide(
    repo: &Repo,
    current: &str,
    branch: &str,
    discard_local: bool,
    purpose: Purpose<'_>,
    ask: impl FnOnce(&str, &str, &'static str) -> Result<bool, PromptError>,
    shutdown: &Shutdown,
) -> anyhow::Result<Decision> {
    let discards = repo.discards(current, branch, &shutdown.token()).await?;
    decide_on(repo, &discards, discard_local, purpose, ask)
}

/// [`decide`] on `discards` already read for `repo` (update's up-front
/// read, without a terminal).
///
/// # Errors
///
/// As [`decide`]'s, except git's reads, already done.
pub(crate) fn decide_on(
    repo: &Repo,
    discards: &Discards,
    discard_local: bool,
    purpose: Purpose<'_>,
    ask: impl FnOnce(&str, &str, &'static str) -> Result<bool, PromptError>,
) -> anyhow::Result<Decision> {
    if discards.is_empty() {
        return Ok(Decision {
            update: true,
            notice: None,
        });
    }
    let name = repo.name();
    let summary = discards.summary();
    // The notice's words for who decided, and the run log's value.
    let (update, decided, logged) = if discard_local {
        (true, "--discard-local", "--discard-local")
    } else {
        let question = format!("{name}: Discard these local changes and {}?", purpose.then);
        match ask(&context(name, discards, purpose), &question, purpose.flags) {
            Ok(answer) => (answer, "your answer", "answer"),
            Err(PromptError::NoTerminal { reason, .. }) => {
                return Err(anyhow::Error::msg(refusal(
                    &reason,
                    &[(name, summary)],
                    purpose,
                )));
            }
            Err(err) => return Err(err.into()),
        }
    };
    log(name, discards, logged, update);
    let notice = if update {
        format!("{name}: Discarding {summary} ({decided})")
    } else {
        format!("{name}: Left as is, keeping {summary} ({decided})")
    };
    Ok(Decision {
        update,
        notice: Some(notice),
    })
}

/// The run log's record of a decision (ops spec, "The run log").
fn log(name: &str, discards: &Discards, decided: &str, discard: bool) {
    tracing::info!(
        repo = name,
        branch = %discards.branch,
        commits = discards.commits,
        detached = discards.detached,
        tracked = discards.changes.tracked.len(),
        untracked = discards.changes.untracked.len(),
        decided,
        discard,
        "local changes"
    );
}

/// The question's context: `<Repo>: <doing> would discard <summary>:`,
/// at most [`NAMES`] names (the tracked ones first), each escaped and
/// cut to [`NAME_WIDTH`] columns, `  and <n> more` for the rest, then
/// what declining means.
pub(crate) fn context(name: &str, discards: &Discards, purpose: Purpose<'_>) -> String {
    let mut lines = vec![format!(
        "{name}: {} would discard {}:",
        purpose.doing,
        discards.summary()
    )];
    let names: Vec<&String> = discards
        .changes
        .tracked
        .iter()
        .chain(&discards.changes.untracked)
        .collect();
    for shown in names.iter().take(NAMES) {
        lines.push(format!(
            "  {}",
            sb_term_ui::fit(&exit::escaped(shown), NAME_WIDTH)
        ));
    }
    if names.len() > NAMES {
        lines.push(format!("  and {} more", names.len() - NAMES));
    }
    lines.push(format!(
        "Declining leaves {name} as it is; {}.",
        purpose.declined
    ));
    lines.join("\n")
}

/// The refusal without a terminal (`reason`, term-ui's): each checkout
/// in `dirty` with its counts, and the flags.
pub(crate) fn refusal(reason: &str, dirty: &[(&str, String)], purpose: Purpose<'_>) -> String {
    format!(
        "can't ask before discarding local changes: {reason}; answer with {}",
        dirty_flags(dirty, purpose.work, purpose.flags)
    )
}

/// `<flags> (<Repo> has <summary> that the <work> would discard; ...)`:
/// the missing answer for checkouts with local changes, as update's
/// up-front refusal and [`refusal`] name it.
pub(crate) fn dirty_flags(dirty: &[(&str, String)], work: &str, flags: &str) -> String {
    let why: Vec<String> = dirty
        .iter()
        .map(|(name, summary)| format!("{name} has {summary} that the {work} would discard"))
        .collect();
    format!("{flags} ({})", why.join("; "))
}

/// What updating `repo` from `current` to `branch` would discard, for
/// the up-front check of a run without a terminal: `None` when git can't
/// read the checkout, whose own step then fails on it.
pub(crate) async fn read(
    repo: &Repo,
    current: &str,
    branch: &str,
    shutdown: &Shutdown,
) -> Option<Discards> {
    match repo.discards(current, branch, &shutdown.token()).await {
        Ok(discards) => Some(discards),
        Err(err) => {
            log_unread(repo, &err);
            None
        }
    }
}

fn log_unread(repo: &Repo, err: &RepoError) {
    tracing::info!(repo = repo.name(), error = %err, "local changes not read up front");
}

/// sb's `warning:` line on `lines`, escaped, in the theme's warn colour
/// (`lines` is anstream's stderr in sb, which strips it when colour is
/// off), and the run log's record.
///
/// # Errors
///
/// cli-core's `OutputError` when `lines` can't be written.
pub(crate) fn warn(lines: &mut dyn Write, text: &str) -> anyhow::Result<()> {
    tracing::warn!(text, "warning");
    let style = theme::current().style(theme::Role::Warn);
    writeln!(lines, "{style}warning:{style:#} {}", exit::escaped(text))
        .and_then(|()| lines.flush())
        .map_err(exit::output_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sb_repo::Changes;

    use super::*;

    const UPDATE: Purpose<'static> = Purpose {
        doing: "updating to 'master'",
        then: "update",
        declined: "the rest of the update goes on",
        work: "update",
        flags: "--discard-local or --skip-git",
    };

    fn discards(tracked: &[&str], untracked: &[&str]) -> Discards {
        let owned = |names: &[&str]| names.iter().map(|&name| name.to_owned()).collect();
        Discards {
            branch: "master".to_owned(),
            commits: 1,
            detached: 0,
            changes: Changes {
                tracked: owned(tracked),
                untracked: owned(untracked),
            },
        }
    }

    #[test]
    fn the_context_names_a_few_files_cut_to_fit() {
        let long = "x".repeat(100);
        let shown = discards(
            &["roles/a.yml", "bad\nname"],
            &["notes.txt", "d/", &long, "z"],
        );
        let text = context("Saltbox", &shown, UPDATE);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "Saltbox: updating to 'master' would discard 1 local commit on 'master', \
             2 tracked changes and 4 untracked files:"
        );
        assert_eq!(
            lines[1..4],
            ["  roles/a.yml", "  bad\\nname", "  notes.txt"]
        );
        assert_eq!(lines[5], format!("  {}…", "x".repeat(77)));
        assert_eq!(lines[6], "  and 1 more");
        assert_eq!(
            lines[7],
            "Declining leaves Saltbox as it is; the rest of the update goes on."
        );
        assert_eq!(lines.len(), 8);
        // Five or fewer: no "more" line.
        let few = context("Sandbox", &discards(&[], &["a"]), UPDATE);
        assert!(!few.contains("more"), "{few}");
    }

    #[test]
    fn a_refusal_names_each_checkout_its_counts_and_the_flags() {
        let text = refusal(
            "no terminal (x)",
            &[
                ("Saltbox", "1 tracked change".to_owned()),
                ("Sandbox", "2 untracked files".to_owned()),
            ],
            UPDATE,
        );
        assert_eq!(
            text,
            "can't ask before discarding local changes: no terminal (x); answer with \
             --discard-local or --skip-git (Saltbox has 1 tracked change that the update \
             would discard; Sandbox has 2 untracked files that the update would discard)"
        );
    }
}
