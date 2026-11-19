//! What updating a checkout would throw away, read before anything
//! changes (repo spec, `discards`, decision 13): the local commits the
//! final reset drops, a detached HEAD's own commits, tracked changes and
//! the untracked files the clean removes. Every read is git's own: `git
//! clean -dn` is the dry run of the clean it predicts
//! (<https://git-scm.com/docs/git-clean#Documentation/git-clean.txt--n>).

use sb_exec::Ending;
use sb_host::child_failure as failure;
use tokio_util::sync::CancellationToken;

use crate::{Repo, RepoError};

/// `git clean -n`'s line for an entry it would remove.
const WOULD_REMOVE: &str = "Would remove ";

/// A checkout's working-tree changes: what `git reset --hard` and `git
/// clean -df` would drop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    /// Tracked paths that differ from HEAD, staged or not.
    pub tracked: Vec<String>,
    /// Untracked files and directories (a directory with its `/`), as
    /// `git clean -dn` names them; ignored files never.
    pub untracked: Vec<String>,
}

impl Changes {
    /// How many: tracked and untracked together.
    #[must_use]
    pub fn count(&self) -> usize {
        self.tracked.len() + self.untracked.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }
}

/// What `fetch_and_reset(branch)` would discard (repo spec, `discards`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discards {
    /// The branch the update resets to.
    pub branch: String,
    /// `branch`'s local commits not on its upstream (as last fetched).
    pub commits: usize,
    /// The commits only a detached HEAD reaches (no branch, remote branch
    /// or tag holds them), which the checkout of `branch` leaves behind.
    pub detached: usize,
    /// The working tree's changes.
    pub changes: Changes,
}

impl Discards {
    /// Whether nothing would be discarded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commits == 0 && self.detached == 0 && self.changes.is_empty()
    }

    /// The counts in words, each part only when not zero: `2 local
    /// commits on 'master', 1 commit only on the detached HEAD, 1 tracked
    /// change and 3 untracked files`; empty when nothing would be
    /// discarded.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.commits > 0 {
            parts.push(format!(
                "{} on '{}'",
                counted(self.commits, "local commit"),
                self.branch
            ));
        }
        if self.detached > 0 {
            parts.push(format!(
                "{} only on the detached HEAD",
                counted(self.detached, "commit")
            ));
        }
        if !self.changes.tracked.is_empty() {
            parts.push(counted(self.changes.tracked.len(), "tracked change"));
        }
        if !self.changes.untracked.is_empty() {
            parts.push(counted(self.changes.untracked.len(), "untracked file"));
        }
        match parts.split_last() {
            None => String::new(),
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        }
    }
}

/// `count` and `thing`, plural when `count` isn't 1.
#[must_use]
pub fn counted(count: usize, thing: &str) -> String {
    if count == 1 {
        format!("1 {thing}")
    } else {
        format!("{count} {thing}s")
    }
}

impl Repo {
    /// The working tree's changes (repo spec, `discards`): `git diff
    /// --name-only -z --no-renames --ignore-submodules=dirty HEAD` for
    /// the tracked ones, staged or not (git's index refresh leaves out a
    /// file touched but unchanged), and `git clean -dn`'s `Would remove`
    /// lines for the untracked ones, exactly what the update's `clean
    /// -df` removes; ignored files are never among them. Local reads, 10
    /// s each.
    ///
    /// # Errors
    ///
    /// [`RepoError::Git`] when git fails or can't run.
    pub async fn local_changes(&self, token: &CancellationToken) -> Result<Changes, RepoError> {
        let diff = self
            .git()
            .arg("diff")
            .arg("--name-only")
            .arg("-z")
            .arg("--no-renames")
            .arg("--ignore-submodules=dirty")
            .arg("HEAD")
            .arg("--");
        let tracked = self.checked(diff, token).await?;
        let tracked = tracked
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        // Names as they are (UTF-8 unescaped, as `diff -z` gives the
        // tracked ones); a control character stays C-quoted by git, and
        // the caller escapes what it shows anyway:
        // <https://git-scm.com/docs/git-config#Documentation/git-config.txt-corequotePath>.
        let clean = self
            .git()
            .arg("-c")
            .arg("core.quotePath=false")
            .arg("clean")
            .arg("-dn");
        let untracked = self.checked(clean, token).await?;
        let untracked = untracked
            .lines()
            .filter_map(|line| line.strip_prefix(WOULD_REMOVE))
            .map(str::to_owned)
            .collect();
        Ok(Changes { tracked, untracked })
    }

    /// What updating the checkout from `current` (its current branch,
    /// `HEAD` when detached) to `branch` would discard (repo spec,
    /// `discards`): [`Repo::local_changes`]; `branch`'s local commits not
    /// on its upstream (`git rev-list --count
    /// <branch>@{u}..refs/heads/<branch>`), none for a branch only on the
    /// remote; and, on a detached HEAD updated to a branch, the commits
    /// only HEAD reaches (`git rev-list --count HEAD --not --branches
    /// --remotes --tags`). A local branch, or the current one, without an
    /// upstream is the update's own refusal, here before any question
    /// (decision 12). Nothing changes but git's index stat cache, which
    /// `git diff` refreshes. `branch` must have passed
    /// [`Repo::check_branch_name`].
    ///
    /// # Errors
    ///
    /// [`RepoError::NoUpstream`] as above; [`RepoError::Git`] when git
    /// fails or can't run.
    pub async fn discards(
        &self,
        current: &str,
        branch: &str,
        token: &CancellationToken,
    ) -> Result<Discards, RepoError> {
        let changes = self.local_changes(token).await?;
        let local = format!("refs/heads/{branch}");
        let upstream = format!("{branch}@{{u}}");
        // A ref that isn't there exits 1; 128 is an upstream that can't be
        // resolved (none configured), as the update's own check reads it
        // (`require_upstream`, whose rule this is).
        let is_local = self.resolves(local.clone(), &[1], token).await?;
        let commits = if is_local || current == branch {
            if !self
                .resolves(upstream.clone(), crate::update::UNRESOLVED, token)
                .await?
            {
                return Err(RepoError::NoUpstream {
                    repo: self.name.clone(),
                    branch: branch.to_owned(),
                });
            }
            let count = self
                .git()
                .arg("rev-list")
                .arg("--count")
                .shown_value(format!("{upstream}..{local}"));
            self.count(count, token).await?
        } else {
            0
        };
        let detached = if current == "HEAD" && branch != "HEAD" {
            let count = self
                .git()
                .arg("rev-list")
                .arg("--count")
                .arg("HEAD")
                .arg("--not")
                .arg("--branches")
                .arg("--remotes")
                .arg("--tags");
            self.count(count, token).await?
        } else {
            0
        };
        Ok(Discards {
            branch: branch.to_owned(),
            commits,
            detached,
            changes,
        })
    }

    /// `cmd`'s stdout, a local read that must exit 0.
    async fn checked(
        &self,
        cmd: sb_exec::Cmd,
        token: &CancellationToken,
    ) -> Result<String, RepoError> {
        let ran = self.read(cmd, token).await?;
        if ran.outcome.ending == Ending::Exited(0) {
            return Ok(ran.stdout);
        }
        Err(RepoError::Git(
            ran.check().err().map_or_default(|f| failure(&f)),
        ))
    }

    /// `git rev-list --count`'s number.
    async fn count(
        &self,
        cmd: sb_exec::Cmd,
        token: &CancellationToken,
    ) -> Result<usize, RepoError> {
        let out = self.checked(cmd, token).await?;
        out.trim()
            .parse()
            .map_err(|_| RepoError::Git(format!("git rev-list --count printed {:?}", out.trim())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discards(commits: usize, detached: usize, tracked: usize, untracked: usize) -> Discards {
        let names = |n: usize| (0..n).map(|i| format!("f{i}")).collect();
        Discards {
            branch: "master".to_owned(),
            commits,
            detached,
            changes: Changes {
                tracked: names(tracked),
                untracked: names(untracked),
            },
        }
    }

    #[test]
    fn the_summary_names_each_count_in_words() {
        assert_eq!(discards(0, 0, 0, 0).summary(), "");
        assert!(discards(0, 0, 0, 0).is_empty());
        assert_eq!(discards(0, 0, 0, 1).summary(), "1 untracked file");
        assert_eq!(
            discards(0, 0, 2, 1).summary(),
            "2 tracked changes and 1 untracked file"
        );
        assert_eq!(
            discards(2, 1, 1, 3).summary(),
            "2 local commits on 'master', 1 commit only on the detached HEAD, \
             1 tracked change and 3 untracked files"
        );
        assert_eq!(
            discards(1, 2, 0, 0).summary(),
            "1 local commit on 'master' and 2 commits only on the detached HEAD"
        );
        for (one, empty) in [
            (discards(1, 0, 0, 0), false),
            (discards(0, 1, 0, 0), false),
            (discards(0, 0, 1, 0), false),
        ] {
            assert_eq!(one.is_empty(), empty, "{one:?}");
        }
        assert_eq!(discards(0, 0, 2, 3).changes.count(), 5);
    }
}
