//! `sb branch <name>` and `sb branch-sandbox <name>` (ops spec, "`sb
//! branch <name>` and `sb branch-sandbox <name>`"; sb-go's cmd/branch.go
//! and cmd/branchSandbox.go): the checkout switched to a branch with
//! sb-repo, owned by the Saltbox user, then Saltbox's runtime and the
//! checkout's tags cache (the spec's steps 1 to 5). Also the branch
//! flags, the branch question and the update step `setup` and `update`
//! share.

use std::io::Write;
use std::path::Path;

use anyhow::Context as _;
use sb_ansible::{Ansible, TagCache};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::Exit;
use sb_cli_core::log::RunLog;
use sb_python_env::{Env, reconcile_runtime};
use sb_repo::{Answer, Choice, Repo, branch_choice, branch_notices};
use sb_term_ui::{Labels, Options, PromptError, Runner, Step};

use crate::Checkout;
use crate::discard::{self, Purpose};

/// The flag that answers the branch commands' local changes question
/// (ops decision 42).
pub const DISCARD_FLAG: &str = "--discard-local";

/// The flags that answer the branch question, as its error names them
/// (ops decision 29).
pub const BRANCH_FLAGS: &str = "--keep-branch or --reset-branch";

// The flags that answer the branch question, `update`'s and `setup`'s
// (ops decision 29): flattened into each command's arguments
// (`#[command(flatten)]`, <https://docs.rs/clap/4.6.7/clap/_derive/index.html>).
// Not a doc comment: clap's derive would take it as the command's
// description, over the variant's (sb's `Command` defers it).
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

/// The branch a command updates a checkout to, and repo's notices for the
/// choice, shown under the command's step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub branch: String,
    pub notices: Vec<String>,
}

/// The branch question for `repo`, on `current`, before any step (repo's
/// `branch_choice`; sb-go's `ResolveUpdateBranch`): `wanted` when the
/// checkout is on it; otherwise `answer` (the flags') decides, or else
/// `ask` shows repo's `<Repo>: Currently on branch '<b>'` and asks `<Repo>:
/// Do you want to reset to the '<wanted>' branch?` (term-ui's confirm in
/// sb, on the terminal), in sb-go's order; the notices are then the
/// answer, `<Repo>: Kept branch '<b>' (your answer)` or `<Repo>: Reset
/// from branch '<b>' to '<wanted>' (your answer)` (ops decision 39), in
/// place of repo's notices for the choice. The chosen branch must pass repo's
/// `check_branch_name` (a kept detached HEAD doesn't).
///
/// # Panics
///
/// Never: repo's `branch_choice` asks only when no answer is given, and
/// the second call has one (a bug otherwise, M-PANIC-ON-BUG).
///
/// # Errors
///
/// The question unanswered (no terminal: term-ui's error naming
/// [`BRANCH_FLAGS`]); a refused branch name.
pub async fn resolve(
    repo: &Repo,
    current: &str,
    wanted: &str,
    answer: Option<Answer>,
    ask: impl FnOnce(&str, &str) -> Result<bool, PromptError>,
    shutdown: &Shutdown,
) -> anyhow::Result<Resolved> {
    // Asked, repo's line for the current branch comes first, as sb-go
    // prints it before its question.
    let shown = branch_notices(repo, current, wanted, &Choice::Ask);
    let (choice, asked) = match branch_choice(current, wanted, answer) {
        Choice::Ask => {
            let question = format!(
                "{}: Do you want to reset to the '{wanted}' branch?",
                repo.name()
            );
            let answer = if ask(&shown.join("\n"), &question)? {
                Answer::Reset
            } else {
                Answer::Keep
            };
            (branch_choice(current, wanted, Some(answer)), true)
        }
        choice => (choice, false),
    };
    let branch = match &choice {
        Choice::Use(branch) | Choice::Keep(branch) => branch.clone(),
        Choice::Ask => unreachable!("branch_choice asked with an answer given"),
    };
    // Asked (on the alternate screen in `update`, ops decision 39), the
    // answer is the notice: it names the branch the question showed.
    let notices = if asked {
        let name = repo.name();
        vec![match choice {
            Choice::Keep(_) => format!("{name}: Kept branch '{branch}' (your answer)"),
            _ => format!("{name}: Reset from branch '{current}' to '{branch}' (your answer)"),
        }]
    } else {
        branch_notices(repo, current, wanted, &choice)
    };
    repo.check_branch_name(&branch, &shutdown.token()).await?;
    Ok(Resolved { branch, notices })
}

/// What a branch switch runs the runtime and the tags with: python-env's
/// layout, release client and programs (Saltbox's runtime), and
/// `ansible-playbook` (both checkouts' tags).
#[derive(Debug, Clone, Copy)]
pub struct Tools<'a> {
    /// python-env's layout, release client and programs.
    pub python: &'a Env<'a>,
    /// `ansible-playbook`.
    pub ansible: &'a Ansible,
}

/// How a branch command is run: `--discard-local`, the question's
/// prompt, the run log, and where a warning line goes.
pub struct Asked<'a, F> {
    /// `--discard-local` given.
    pub discard_local: bool,
    /// The local changes question (term-ui's `confirm_over` in sb):
    /// context, question, flag.
    pub ask: F,
    /// The run log for the runner's records.
    pub log: Option<&'a RunLog>,
    /// sb's lines (anstream's stderr in sb): a declined switch's warning.
    pub lines: &'a mut dyn Write,
}

impl<F> std::fmt::Debug for Asked<'_, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asked")
            .field("discard_local", &self.discard_local)
            .finish_non_exhaustive()
    }
}

/// The branch command for `checkout`, under the Saltbox directory `root`:
/// the checkout opened, `name` checked (repo's `check_branch_name`), the
/// Saltbox user read (config's `saltbox_user`), then the local changes
/// the switch would discard (decision 42: `--discard-local`, else
/// `asked.ask`; no terminal, a refusal naming them and
/// [`DISCARD_FLAG`]), all before anything changes and failing as sb's
/// error block; then [`switch`] in a runner, the decision a notice under
/// its step. Declined, nothing runs: the warning `<Repo> left as is: not
/// switched to '<name>' (your answer)`, and sb ends well. No branch
/// prompt: the argument is the answer (spec, decision 20).
///
/// # Errors
///
/// The checkout missing or not a directory, a refused branch name, a
/// missing Saltbox user, the checkout unreadable, the question
/// unanswered, or the switch's failed step (cli-core's quiet `Exit`, its
/// block shown by the runner).
pub async fn run(
    checkout: Checkout,
    root: &Path,
    name: &str,
    tools: Tools<'_>,
    shutdown: &Shutdown,
    asked: Asked<'_, impl FnOnce(&str, &str, &'static str) -> Result<bool, PromptError>>,
) -> anyhow::Result<()> {
    let repo = Repo::open(checkout.name(), checkout.path(root))?;
    repo.check_branch_name(name, &shutdown.token()).await?;
    let owner = sb_config::user::saltbox_user(root)?;
    let current = repo.current_branch(&shutdown.token()).await?;
    let (doing, then) = (
        format!("switching to '{name}'"),
        format!("switch to '{name}'"),
    );
    let purpose = Purpose {
        doing: &doing,
        then: &then,
        declined: "nothing is switched",
        work: "switch",
        flags: DISCARD_FLAG,
    };
    let decision = discard::decide(
        &repo,
        &current,
        name,
        asked.discard_local,
        purpose,
        asked.ask,
        shutdown,
    )
    .await?;
    if !decision.update {
        return discard::warn(
            asked.lines,
            &format!(
                "{} left as is: not switched to '{name}' (your answer)",
                repo.name()
            ),
        );
    }
    let mut runner = Runner::start(checkout.command(), shutdown, asked.log, Options::default());
    let switching = Switching {
        checkout,
        repo: &repo,
        branch: name,
        owner: &owner,
        notice: decision.notice.as_deref(),
    };
    let result = switch(&mut runner, &switching, root, tools).await;
    runner.finish();
    Ok(result?)
}

/// The switch [`switch`] makes: the checkout, to which branch, its owner
/// after it, and the local changes notice.
#[derive(Debug, Clone, Copy)]
struct Switching<'a> {
    checkout: Checkout,
    repo: &'a Repo,
    branch: &'a str,
    owner: &'a str,
    notice: Option<&'a str>,
}

/// The switch under sb-go's labels (spec, steps 3 to 5): "Switching
/// <Repo> repository to <name>" around "Updating <Repo> repository"
/// (repo's `fetch_and_reset`, which widens the refspec, checks the branch
/// is on the remote and sets the ownership), then, for Saltbox,
/// python-env's `reconcile_runtime` as `update` runs it (decision 21),
/// then the checkout's tags cache rebuilt, always, as sb-go.
///
/// # Errors
///
/// The failed step's quiet `Exit`.
async fn switch(
    runner: &mut Runner,
    switching: &Switching<'_>,
    root: &Path,
    tools: Tools<'_>,
) -> Result<(), Exit> {
    let Switching {
        checkout,
        repo,
        branch,
        owner,
        notice,
    } = *switching;
    let repo_name = repo.name();
    let labels = Labels::new(
        format!("Switching {repo_name} repository to {branch}"),
        format!("{repo_name} repository switched to {branch}"),
    )
    .failure(format!("{repo_name} branch switch"));
    runner
        .step(labels, async |step| {
            if let Some(notice) = notice {
                step.info(notice);
            }
            update(step, repo, branch, owner).await?;
            if checkout == Checkout::Saltbox {
                reconcile_runtime(step, tools.python, Some(owner)).await?;
            }
            let mut cache = TagCache::load(root);
            tags(
                step,
                &mut cache,
                tools.ansible,
                &checkout.playbooks(root),
                repo,
            )
            .await?;
            Ok(())
        })
        .await
}

/// The checkout's tags rebuilt into the cache (ansible's
/// `TagCache::refresh`, under its "Updating <Repo> tags cache" sub-step),
/// sb-go's words around a failure; the tags listed.
///
/// # Errors
///
/// The rebuild's error, its block shown in its sub-step.
pub(crate) async fn tags(
    step: &mut Step,
    cache: &mut TagCache,
    ansible: &Ansible,
    playbooks: &sb_ansible::Repo,
    repo: &Repo,
) -> anyhow::Result<Vec<String>> {
    cache
        .refresh(ansible, playbooks, repo, step)
        .await
        .context("error running and caching ansible tags")
}

/// The labels of [`update`]'s step for `repo_name` updated to `branch`
/// (update's concurrent git phases take them too: ops spec, step 7).
pub(crate) fn repository_labels(repo_name: &str, branch: &str) -> Labels {
    Labels::new(
        format!("Updating {repo_name} repository"),
        format!("{repo_name} repository updated ({branch})"),
    )
    .failure(format!("{repo_name} repository update"))
}

/// "Updating <Repo> repository" (success "<Repo> repository updated
/// (<branch>)"), repo's `fetch_and_reset` to `branch`, owned by `owner`:
/// sb-go's step in the branch commands and in setup over a checkout
/// (saltbox/setup.go's `SaltboxRepo`).
///
/// # Errors
///
/// The failed step's quiet `Exit`.
pub(crate) async fn update(
    step: &mut Step,
    repo: &Repo,
    branch: &str,
    owner: &str,
) -> Result<(), Exit> {
    step.step(repository_labels(repo.name(), branch), async |step| {
        repo.fetch_and_reset(branch, owner, step).await
    })
    .await
}
