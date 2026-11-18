//! The git child (repo spec, "Remote operations" and decision 4): the
//! host's git in the checkout, with exec's environment rules
//! (`GIT_TERMINAL_PROMPT=0`, ssh's failing askpass) plus `LC_ALL=C`, so
//! git's messages are the same in every locale.

use std::time::Duration;

use sb_cli_core::config::MAX_FILE_BYTES;
use sb_exec::{Captured, Cmd, Ending};
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

    /// The text of the file at `path` (relative to the checkout's top) in
    /// the commit `reference` names (repo spec, amended 2026-11-18 for
    /// ops' `announcements preview`): `git rev-parse --verify --quiet
    /// <ref>^{commit}` (why not `--end-of-options`: a ref starting with `-`
    /// is refused first, as branch names are, repo decision 3), then `git
    /// ls-tree` for the path (a blob, or none: a directory or a submodule
    /// there is no file), `git cat-file -s` and `git cat-file blob` of
    /// `<commit>:<path>`, each a local read within 10 s. `None` when that
    /// commit has no such file. The text is held to cli-core's rules for
    /// a small file: at most [`MAX_FILE_BYTES`], UTF-8. exec's capture
    /// keeps text and writes U+FFFD for bytes that aren't UTF-8, so a
    /// U+FFFD counts as not UTF-8 here.
    ///
    /// # Errors
    ///
    /// [`RepoError::Ref`] for a ref starting with `-` or one git doesn't
    /// know as a commit (`rev-parse` exiting 1); [`RepoError::Blob`] for a file too big or not
    /// UTF-8; [`RepoError::Git`] when git can't run or fails otherwise.
    pub async fn file_at(
        &self,
        reference: &str,
        path: &str,
        token: &CancellationToken,
    ) -> Result<Option<String>, RepoError> {
        let unknown = || RepoError::Ref {
            reference: reference.escape_debug().to_string(),
            path: self.path.clone(),
        };
        if reference.starts_with('-') {
            return Err(unknown());
        }
        let cmd = self
            .git()
            .arg("rev-parse")
            .arg("--verify")
            .arg("--quiet")
            .shown_value(format!("{reference}^{{commit}}"));
        // `--verify --quiet` exits 1 for a ref that isn't a commit; any
        // other failure (no repository, git's dubious-ownership refusal,
        // 128; a timeout) is git's own, its words kept.
        let commit = self.read(cmd, token).await?;
        if commit.outcome.ending == Ending::Exited(1) {
            return Err(unknown());
        }
        let commit = commit
            .check()
            .map_err(|failed| RepoError::Git(failure(&failed)))?;
        let object = format!("{}:{path}", commit.stdout.trim());
        // Whether the commit has the file: `ls-tree` lists the path, a
        // blob, or nothing (exit 0 either way); then `cat-file` failing is
        // git's failure, never "no file".
        let cmd = self
            .git()
            .arg("ls-tree")
            .shown_value(commit.stdout.trim())
            .arg("--")
            .shown_value(path);
        let listed = self.read(cmd, token).await?;
        let listed = listed
            .check()
            .map_err(|failed| RepoError::Git(failure(&failed)))?;
        let blob = listed.stdout.split_whitespace().nth(1) == Some("blob");
        if !blob {
            return Ok(None);
        }
        let shown = format!("{reference}:{path}");
        let cmd = self.git().arg("cat-file").arg("-s").shown_value(&object);
        let size = self.read(cmd, token).await?;
        let size = size
            .check()
            .map_err(|failed| RepoError::Git(failure(&failed)))?;
        let size: usize =
            size.stdout.trim().parse().map_err(|_| {
                RepoError::Git(format!("git cat-file -s {shown}: {:?}", size.stdout))
            })?;
        if size > MAX_FILE_BYTES {
            return Err(RepoError::Blob(format!(
                "{shown} is bigger than {MAX_FILE_BYTES} bytes"
            )));
        }
        let cmd = self.git().arg("cat-file").arg("blob").shown_value(&object);
        let blob = self.read(cmd, token).await?;
        let blob = blob
            .check()
            .map_err(|failed| RepoError::Git(failure(&failed)))?;
        if blob.stdout.len() != size || blob.stdout.contains('\u{fffd}') {
            return Err(RepoError::Blob(format!("{shown} isn't UTF-8")));
        }
        Ok(Some(blob.stdout))
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
            .shown_value(name);
        let ran = self.read(cmd, token).await?;
        match ran.check() {
            Ok(ran) if ran.stdout.trim_end_matches('\n') == name => Ok(()),
            _ => Err(refused()),
        }
    }
}
