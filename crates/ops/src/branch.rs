//! `sb branch <name>` and `sb branch-sandbox <name>` (ops spec, "`sb
//! branch <name>` and `sb branch-sandbox <name>`"; sb-go's cmd/branch.go
//! and cmd/branchSandbox.go): the checkout switched to a branch with
//! sb-repo, owned by the Saltbox user. The spec's steps 1 to 3; the
//! runtime and the tags cache (its steps 4 and 5) join when those land
//! (module build M85).

use std::path::{Path, PathBuf};

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::exit::Exit;
use sb_cli_core::log::RunLog;
use sb_repo::{Answer, Choice, Repo, branch_choice, branch_notices};
use sb_term_ui::{Labels, Options, PromptError, Runner, Step};

/// Which checkout a branch command switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkout {
    /// Saltbox's own checkout, the Saltbox directory itself (`sb branch`).
    Saltbox,
    /// The Sandbox checkout, `<server_appdata_path>/sandbox` (`sb
    /// branch-sandbox`).
    Sandbox,
}

impl Checkout {
    /// The checkout's name in labels and errors (sb-go's).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Saltbox => "Saltbox",
            Self::Sandbox => "Sandbox",
        }
    }

    /// The command, for the runner's log record.
    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Self::Saltbox => "sb branch",
            Self::Sandbox => "sb branch-sandbox",
        }
    }

    /// Where the checkout is, under the Saltbox directory `root`: `root`
    /// itself, or the Sandbox under the inventory's appdata path (as sb
    /// finds its theme; cli-core's `appdata_path`).
    #[must_use]
    pub fn path(self, root: &Path) -> PathBuf {
        match self {
            Self::Saltbox => root.to_path_buf(),
            Self::Sandbox => config::appdata_path(&config::inventory_path(root)).join("sandbox"),
        }
    }
}

/// The flags that answer the branch question, as its error names them
/// (ops decision 29).
pub const BRANCH_FLAGS: &str = "--keep-branch or --reset-branch";

/// The flags that answer the branch question, `update`'s and `setup`'s
/// (ops decision 29): flattened into each command's arguments
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
/// sb, on the terminal), in sb-go's order; the notices then leave out the
/// line already shown. The chosen branch must pass repo's
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
    let mut notices = branch_notices(repo, current, wanted, &choice);
    if asked {
        notices.retain(|notice| !shown.contains(notice));
    }
    let branch = match choice {
        Choice::Use(branch) | Choice::Keep(branch) => branch,
        Choice::Ask => unreachable!("branch_choice asked with an answer given"),
    };
    repo.check_branch_name(&branch, &shutdown.token()).await?;
    Ok(Resolved { branch, notices })
}

/// The branch command for `checkout`, under the Saltbox directory `root`:
/// the checkout opened, `name` checked (repo's `check_branch_name`), the
/// Saltbox user read (config's `saltbox_user`), all before anything
/// changes and failing as sb's error block; then the checkout fetched and
/// reset to `name`, owned by that user, in a runner step under sb-go's
/// labels. No prompt: the argument is the answer (spec, decision 20).
///
/// # Errors
///
/// The checkout missing or not a directory, a refused branch name, a
/// missing Saltbox user, or the switch's failed step (cli-core's quiet
/// `Exit`, its block shown by the runner).
pub async fn run(
    checkout: Checkout,
    root: &Path,
    name: &str,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let repo = Repo::open(checkout.name(), checkout.path(root))?;
    repo.check_branch_name(name, &shutdown.token()).await?;
    let owner = sb_config::user::saltbox_user(root)?;
    let mut runner = Runner::start(checkout.command(), shutdown, log, Options::default());
    let result = switch(&mut runner, &repo, name, &owner).await;
    runner.finish();
    Ok(result?)
}

/// The switch under sb-go's labels, named by the checkout (`repo.name()`,
/// as repo's own errors are): "Switching <Repo> repository to <name>"
/// around "Updating <Repo> repository", repo's `fetch_and_reset` (which
/// widens the refspec, checks the branch is on the remote and sets the
/// ownership). Crate-private until update's steps call it (M84, M85).
///
/// # Errors
///
/// The failed step's quiet `Exit`.
pub(crate) async fn switch(
    runner: &mut Runner,
    repo: &Repo,
    branch: &str,
    owner: &str,
) -> Result<(), Exit> {
    let repo_name = repo.name();
    let labels = Labels::new(
        format!("Switching {repo_name} repository to {branch}"),
        format!("{repo_name} repository switched to {branch}"),
    )
    .failure(format!("{repo_name} branch switch"));
    runner
        .step(labels, async |step| {
            update(step, repo, branch, owner).await?;
            Ok(())
        })
        .await
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
    let repo_name = repo.name();
    let updating = Labels::new(
        format!("Updating {repo_name} repository"),
        format!("{repo_name} repository updated ({branch})"),
    )
    .failure(format!("{repo_name} repository update"));
    step.step(updating, async |step| {
        repo.fetch_and_reset(branch, owner, step).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_checkout_has_sb_gos_name_and_its_command() {
        assert_eq!(Checkout::Saltbox.name(), "Saltbox");
        assert_eq!(Checkout::Sandbox.name(), "Sandbox");
        assert_eq!(Checkout::Saltbox.command(), "sb branch");
        assert_eq!(Checkout::Sandbox.command(), "sb branch-sandbox");
    }

    #[test]
    fn the_saltbox_checkout_is_the_root_and_the_sandbox_is_under_appdata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("saltbox");
        std::fs::create_dir_all(root.join("inventories/host_vars")).expect("inventory dir");
        std::fs::write(
            root.join("inventories/host_vars/localhost.yml"),
            "server_appdata_path: /data/appdata\n",
        )
        .expect("inventory");
        assert_eq!(Checkout::Saltbox.path(&root), root);
        assert_eq!(
            Checkout::Sandbox.path(&root),
            Path::new("/data/appdata/sandbox")
        );
    }
}
