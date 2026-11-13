//! Updating a checkout (repo spec, "Operations"; sb-go's git/git.go):
//! the fetch refspec, then fetch, reset to a branch, submodules and
//! ownership, each a sub-step with sb-go's labels.

use sb_exec::Ending;
use sb_term_ui::{Labels, Step};
use tokio_util::sync::CancellationToken;

use crate::{Repo, RepoError};
use sb_host::child_failure as failure;

/// The refspec that fetches every branch.
const ALL_BRANCHES: &str = "+refs/heads/*:refs/remotes/origin/*";

impl Repo {
    /// Make `origin` fetch every branch (sb-go's
    /// `EnsureRemoteFetchAllBranches`, 2f2ac78): when no
    /// `remote.origin.fetch` value contains
    /// `refs/heads/*:refs/remotes/origin/*`, add
    /// `+refs/heads/*:refs/remotes/origin/*`. `git config --get-all` exits
    /// 1 for an unset key: no values, not an error.
    ///
    /// # Errors
    ///
    /// [`RepoError::ReadFetch`] or [`RepoError::UpdateFetch`] with git's
    /// failure.
    pub async fn ensure_fetch_all_branches(
        &self,
        token: &CancellationToken,
    ) -> Result<(), RepoError> {
        let get = self
            .git()
            .arg("config")
            .arg("--get-all")
            .arg("remote.origin.fetch");
        let ran = self
            .read(get, token)
            .await
            .map_err(|err| RepoError::ReadFetch(err.to_string()))?;
        if ran.outcome.ending != Ending::Exited(1) {
            let ran = ran
                .check()
                .map_err(|failed| RepoError::ReadFetch(failure(&failed)))?;
            if ran
                .stdout
                .lines()
                .any(|line| line.contains("refs/heads/*:refs/remotes/origin/*"))
            {
                return Ok(());
            }
        }
        let add = self
            .git()
            .arg("config")
            .arg("--add")
            .arg("remote.origin.fetch")
            .arg(ALL_BRANCHES);
        let ran = self
            .read(add, token)
            .await
            .map_err(|err| RepoError::UpdateFetch(err.to_string()))?;
        let _added = ran
            .check()
            .map_err(|failed| RepoError::UpdateFetch(failure(&failed)))?;
        tracing::info!(
            repo = self.name.as_str(),
            "fetch refspec widened to every branch"
        );
        Ok(())
    }

    /// Update the checkout to `branch` under `step` (sb-go's
    /// `FetchAndResetBranch`, git/git.go:196-213, with the spec's
    /// differences):
    ///
    /// 1. the fetch refspec widened ([`Repo::ensure_fetch_all_branches`],
    ///    decision 6);
    /// 2. "Fetching repository changes": `git fetch --progress`;
    /// 3. a branch other than the current one must be on the remote,
    ///    checked before the worktree changes (decision 8);
    /// 4. "Resetting repository to `branch`": clean, reset to `HEAD`
    ///    (decision 7), `checkout <branch> --`, clean, reset to `@{u}`;
    ///    never `clean -x`, so ignored files (Saltbox's config) stay;
    /// 5. "Updating git submodules";
    /// 6. "Setting repository ownership": host's
    ///    `ensure_owner_recursive(owner, [path])`.
    ///
    /// `branch` must have passed [`Repo::check_branch_name`]; one starting
    /// with `-` is refused here as well, so it never reaches git as an
    /// option whoever calls.
    ///
    /// # Errors
    ///
    /// The first failing sub-step's: a [`RepoError`], exec's `Failed` or
    /// spawn error, or host's ownership error.
    pub async fn fetch_and_reset(
        &self,
        branch: &str,
        owner: &str,
        step: &mut Step,
    ) -> anyhow::Result<()> {
        if branch.starts_with('-') {
            return Err(RepoError::BranchName(branch.escape_debug().to_string()).into());
        }
        let token = step.token();
        // The current branch first: on a checkout root's git refuses (no
        // safe.directory entry), its error is git's "dubious ownership",
        // where `git config` would only say "not in a git directory".
        let current = self.current_branch(&token).await?;
        self.ensure_fetch_all_branches(&token).await?;
        let fetching = "Fetching repository changes";
        step.step(Labels::new(fetching, fetching), async |step| {
            self.change(step, self.git().arg("fetch").arg("--progress"))
                .await
        })
        .await?;
        if current != branch {
            self.require_remote_branch(branch, &token).await?;
        }
        let resetting = format!("Resetting repository to {branch}");
        step.step(Labels::new(&resetting, &resetting), async |step| {
            self.reset_to(step, branch).await
        })
        .await?;
        let submodules = "Updating git submodules";
        step.step(Labels::new(submodules, submodules), async |step| {
            let cmd = self
                .git()
                .arg("submodule")
                .arg("update")
                .arg("--progress")
                .arg("--init")
                .arg("--recursive");
            self.change(step, cmd).await
        })
        .await?;
        let owning = "Setting repository ownership";
        step.step(Labels::new(owning, owning), async |_| {
            let (owner, path) = (owner.to_string(), self.path.clone());
            // The walk blocks (thousands of entries), off the runner's
            // thread (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
            tokio::task::spawn_blocking(move || sb_host::ensure_owner_recursive(&owner, &[path]))
                .await??;
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// `branch` on the remote: `git rev-parse --verify --quiet
    /// refs/remotes/origin/<branch>`.
    async fn require_remote_branch(
        &self,
        branch: &str,
        token: &CancellationToken,
    ) -> Result<(), RepoError> {
        let cmd = self
            .git()
            .arg("rev-parse")
            .arg("--verify")
            .arg("--quiet")
            .value(format!("refs/remotes/origin/{branch}"));
        let ran = self.read(cmd, token).await?;
        match ran.outcome.ending {
            Ending::Exited(0) => Ok(()),
            Ending::Exited(1) => Err(RepoError::MissingBranch {
                branch: branch.to_string(),
                repo: self.name.clone(),
            }),
            _ => Err(RepoError::Git(
                ran.check().err().map_or_default(|failed| failure(&failed)),
            )),
        }
    }

    /// The reset sub-step's commands (sb-go's `resetCommands`, with the
    /// reset before checkout to `HEAD` and `--` after the branch).
    async fn reset_to(&self, step: &mut Step, branch: &str) -> anyhow::Result<()> {
        let clean = || self.git().arg("clean").arg("--quiet").arg("-df");
        self.change(step, clean()).await?;
        let reset = self.git().arg("reset").arg("--quiet").arg("--hard");
        self.change(step, reset).await?;
        let checkout = self
            .git()
            .arg("checkout")
            .arg("--quiet")
            .value(branch)
            .arg("--");
        self.change(step, checkout).await?;
        self.change(step, clean()).await?;
        let upstream = self
            .git()
            .arg("rev-parse")
            .arg("--verify")
            .arg("--quiet")
            .arg("@{u}");
        let ran = self.read(upstream, &step.token()).await?;
        match ran.outcome.ending {
            Ending::Exited(0) => {}
            // `--verify --quiet` exits 1 when `@{u}` names nothing, 128
            // when it can't be resolved at all (no upstream configured).
            Ending::Exited(1 | 128) => {
                return Err(RepoError::NoUpstream {
                    repo: self.name.clone(),
                    branch: branch.to_string(),
                }
                .into());
            }
            // Interrupted, timed out or killed: exec's own text.
            _ => {
                return Err(
                    RepoError::Git(ran.check().err().map_or_default(|f| failure(&f))).into(),
                );
            }
        }
        let reset = self
            .git()
            .arg("reset")
            .arg("--quiet")
            .arg("--hard")
            .arg("@{u}");
        self.change(step, reset).await
    }
}
