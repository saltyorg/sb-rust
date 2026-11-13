//! The git child (repo spec, "Remote operations" and decision 4): the
//! host's git in the checkout, with exec's environment rules
//! (`GIT_TERMINAL_PROMPT=0`, ssh's failing askpass) plus `LC_ALL=C`, so
//! git's messages are the same in every locale.

use std::time::Duration;

use sb_exec::{Captured, Cmd};
use sb_host::child_failure as failure;
use sb_term_ui::Step;
use tokio_util::sync::CancellationToken;

use crate::{Repo, RepoError};

/// A local read's limit.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

impl Repo {
    /// `git` in the checkout, with `LC_ALL=C`.
    pub(crate) fn git(&self) -> Cmd {
        Cmd::new(self.git.as_os_str())
            .cwd(&self.path)
            .env("LC_ALL", "C")
    }

    /// `cmd`, a local read, captured within 10 s.
    pub(crate) async fn read(
        &self,
        cmd: Cmd,
        token: &CancellationToken,
    ) -> Result<Captured, RepoError> {
        cmd.timeout(READ_TIMEOUT)
            .capture(token)
            .await
            .map_err(|err| RepoError::Git(format!("{err}: {}", err.cause())))
    }

    /// `cmd`, a change, streamed into `step` with no timeout: Ctrl-C and
    /// SIGTERM stop it through exec's process group (exec spec, `stream`).
    ///
    /// # Errors
    ///
    /// exec's spawn error or [`Failed`] (the command and its exit; the
    /// step shows git's last lines).
    pub(crate) async fn change(&self, step: &mut Step, cmd: Cmd) -> anyhow::Result<()> {
        // The output is in the step and the run log already.
        let _streamed = cmd.stream(step).await?.check()?;
        Ok(())
    }

    /// The current branch: `git rev-parse --abbrev-ref HEAD`, `HEAD` when
    /// detached (sb-go's git/git.go:89-97).
    ///
    /// # Errors
    ///
    /// [`RepoError::CurrentBranch`] with git's failure.
    pub async fn current_branch(&self, token: &CancellationToken) -> Result<String, RepoError> {
        let cmd = self.git().arg("rev-parse").arg("--abbrev-ref").arg("HEAD");
        let ran = self
            .read(cmd, token)
            .await
            .map_err(|err| RepoError::CurrentBranch(err.to_string()))?;
        let ran = ran
            .check()
            .map_err(|failed| RepoError::CurrentBranch(failure(&failed)))?;
        Ok(ran.stdout.trim().to_string())
    }

    /// The checked-out commit: `git rev-parse HEAD`, captured within 10 s
    /// (repo spec, `head()`). Callers compare it before and after an
    /// update.
    ///
    /// # Errors
    ///
    /// [`RepoError::Head`] with git's failure: an unborn branch, no
    /// repository, or no git.
    pub async fn head(&self, token: &CancellationToken) -> Result<String, RepoError> {
        let cmd = self.git().arg("rev-parse").arg("HEAD");
        let ran = self
            .read(cmd, token)
            .await
            .map_err(|err| RepoError::Head(err.to_string()))?;
        let ran = ran
            .check()
            .map_err(|failed| RepoError::Head(failure(&failed)))?;
        Ok(ran.stdout.trim().to_string())
    }

    /// Refuse a branch name before it reaches any other git command
    /// (repo spec, decision 3): one starting with `-` (git would read it
    /// as an option), and one `git check-ref-format --branch` rejects or
    /// rewrites (it expands `@{-1}`; the printed name must be the given
    /// one).
    ///
    /// # Errors
    ///
    /// [`RepoError::BranchName`], the name escaped; [`RepoError::Git`]
    /// when git can't run.
    pub async fn check_branch_name(
        &self,
        name: &str,
        token: &CancellationToken,
    ) -> Result<(), RepoError> {
        let refused = || RepoError::BranchName(name.escape_debug().to_string());
        if name.starts_with('-') {
            return Err(refused());
        }
        let cmd = self
            .git()
            .arg("check-ref-format")
            .arg("--branch")
            .value(name);
        let ran = self.read(cmd, token).await?;
        match ran.check() {
            Ok(ran) if ran.stdout.trim_end_matches('\n') == name => Ok(()),
            _ => Err(refused()),
        }
    }
}
