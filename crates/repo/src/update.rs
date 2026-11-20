//! Updating a checkout (repo spec, "Operations"; sb-go's git/git.go):
//! the fetch refspec, then fetch, reset to a branch, submodules and
//! ownership, each a sub-step with sb-go's labels.

use std::io;
use std::path::PathBuf;

use anyhow::Context as _;
use sb_exec::Ending;
use sb_term_ui::{Labels, Step};
use tokio_util::sync::CancellationToken;

use crate::{Repo, RepoError};
use sb_host::child_failure as failure;

/// `git rev-parse --verify --quiet`'s exits for an upstream that names
/// nothing: 1 when `@{u}` resolves to no commit (its remote branch gone),
/// 128 when it can't be resolved at all (no upstream configured, no such
/// branch).
pub(crate) const UNRESOLVED: &[i32] = &[1, 128];

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
    /// 3. a branch other than the current one must be on the remote, and
    ///    `branch`, when it is a local branch (the current one always),
    ///    must have an upstream, both checked before the worktree changes
    ///    (decision 8; "Order": a refused run leaves the checkout as it
    ///    was);
    /// 4. "Resetting repository to `branch`": clean, reset to `HEAD`
    ///    (decision 7), `checkout <branch> --`, clean, reset to `@{u}`;
    ///    never `clean -x`, so ignored files (Saltbox's config) stay;
    /// 5. "Updating git submodules", when the checkout has a
    ///    `.gitmodules` (difference 15);
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
            self.remote(step, &["fetch", "--progress"]).await
        })
        .await?;
        if current != branch {
            self.require_remote_branch(branch, &token).await?;
        }
        self.require_upstream(branch, current == branch, &token)
            .await?;
        let resetting = format!("Resetting repository to {branch}");
        step.step(Labels::new(&resetting, &resetting), async |step| {
            self.reset_to(step, branch).await
        })
        .await?;
        let submodules = "Updating git submodules";
        step.step(Labels::new(submodules, submodules), async |step| {
            // Without a `.gitmodules` git has no submodule to update, and
            // its `submodule` is a shell script: 20 programs, 23-25 ms per
            // checkout with git 2.34 and 2.43 (measured 2026-11-20;
            // difference 15). Anything but a plain "not there" runs it.
            if let Err(err) = self.path.join(".gitmodules").symlink_metadata()
                && err.kind() == std::io::ErrorKind::NotFound
            {
                tracing::info!(repo = %self.name, "no .gitmodules: submodule update not run");
                return Ok(());
            }
            self.remote(
                step,
                &["submodule", "update", "--progress", "--init", "--recursive"],
            )
            .await
        })
        .await?;
        let owning = "Setting repository ownership";
        step.step(Labels::new(owning, owning), async |_| {
            let (owner, path) = (owner.to_string(), self.path.clone());
            // The walk blocks (thousands of entries), off the runner's
            // thread (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>),
            // with the run's span, so its record carries the run id
            // (cli-core's `spawn`).
            sb_cli_core::spawn::blocking(move || sb_host::ensure_owner_recursive(&owner, &[path]))
                .await??;
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// Clone `url` at `branch` into `path`, which must not exist, for `sb
    /// setup` (repo spec, "Operations"; sb-go's `CloneRepository`,
    /// git/git.go:18-43, and setup's submodule step, saltbox/setup.go:205-220):
    ///
    /// 1. the branch name checked ([`Repo::check_branch_name`]) before
    ///    anything runs;
    /// 2. "Cloning `<name>` repository to `<path>` (branch: `<branch>`)":
    ///    `git clone --progress --depth 1 -b <branch> <url> <path>`, a
    ///    remote operation;
    /// 3. "Updating git submodules".
    ///
    /// A failed clone leaves no checkout: git removes the directory it
    /// made, though not the parents it made for it. A failed submodule
    /// update leaves the checkout, as sb-go does. The clone and the
    /// submodule update are two steps, not `git clone
    /// --recurse-submodules`, so each has sb-go's label, error and HTTP/1.1
    /// retry. git runs from `/`, so a relative `url` would resolve there
    /// (setup passes Saltbox's https URL).
    ///
    /// An associated function, so `Repo::clone(&repo)` doesn't name
    /// `Clone::clone`; `repo.clone()` still does.
    ///
    /// # Errors
    ///
    /// [`RepoError::BranchName`], [`RepoError::Destination`] when `path`
    /// is there, [`RepoError::Inspect`] when it can't be inspected or
    /// isn't UTF-8. A failed step shows its error in the step, under
    /// sb-go's `failed to clone <name> repository '<url>' (branch:
    /// '<branch>') to '<path>'` or `error running git submodule update`,
    /// and the caller gets term-ui's exit.
    pub async fn clone(
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        url: &str,
        branch: &str,
        step: &mut Step,
    ) -> anyhow::Result<Self> {
        Self::clone_using(PathBuf::from("git"), name, path, url, branch, step).await
    }

    /// [`Repo::clone`] running `git` as git (tests).
    ///
    /// # Errors
    ///
    /// As [`Repo::clone`].
    #[cfg(any(test, feature = "testing"))]
    pub async fn clone_with_git(
        git: impl Into<PathBuf>,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        url: &str,
        branch: &str,
        step: &mut Step,
    ) -> anyhow::Result<Self> {
        Self::clone_using(git.into(), name, path, url, branch, step).await
    }

    async fn clone_using(
        git: PathBuf,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        url: &str,
        branch: &str,
        step: &mut Step,
    ) -> anyhow::Result<Self> {
        let name = name.into();
        let path = path.into();
        // git runs from `/`: the destination's parents may not exist yet
        // (git clone makes them), and sb's own directory is no concern of
        // the clone's.
        let outside = Self {
            name: name.clone(),
            path: PathBuf::from("/"),
            git: git.clone(),
        };
        outside.check_branch_name(branch, &step.token()).await?;
        // Absolute, since git runs from `/`
        // (<https://doc.rust-lang.org/std/path/fn.absolute.html>).
        let path = std::path::absolute(&path).map_err(|cause| RepoError::Inspect {
            path: path.clone(),
            cause,
        })?;
        // Not followed: a dangling symlink is there too.
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Err(RepoError::Destination(path).into()),
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
            Err(cause) => return Err(RepoError::Inspect { path, cause }.into()),
        }
        let Some(destination) = path.to_str() else {
            let cause = io::Error::new(io::ErrorKind::InvalidInput, "the path isn't UTF-8");
            return Err(RepoError::Inspect { path, cause }.into());
        };
        let cloning = format!("Cloning {name} repository to {destination} (branch: {branch})");
        step.step(Labels::new(&cloning, &cloning), async |step| {
            outside
                .remote_with(
                    step,
                    &["clone", "--progress", "--depth", "1", "-b"],
                    &[branch, url, destination],
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to clone {name} repository '{url}' (branch: '{branch}') to \
                         '{destination}'"
                    )
                })
        })
        .await?;
        let repo = Self {
            name,
            path: path.clone(),
            git,
        };
        repo.submodules(step, "error running git submodule update")
            .await?;
        Ok(repo)
    }

    /// Make the existing directory a checkout of `url` at `branch`, for
    /// `sb setup --force` over a root without `.git` (repo spec,
    /// "Operations"; sb-go's saltbox/setup.go:235-280). Under
    /// "Initializing Git repository", each a sub-step with sb-go's label:
    /// `git init -b <branch>` (sb-go: `git init`, which left HEAD on git's
    /// default branch; difference 12), `git remote add origin <url>`, `git fetch --progress
    /// --all --prune` (a remote operation), `git branch <branch>
    /// origin/<branch>`, `git reset --hard origin/<branch>`, and the
    /// submodule update. The branch name is checked first.
    ///
    /// A failure leaves what the steps before it made, as sb-go does: a
    /// rerun of `setup --force` then finds a checkout and fetches and
    /// resets it (repo spec, Known limits).
    ///
    /// # Errors
    ///
    /// [`RepoError::BranchName`]. A failed step shows its error in the
    /// step, under sb-go's `error running command [git <args>]`, and the
    /// caller gets term-ui's exit.
    pub async fn init_from_remote(
        &self,
        url: &str,
        branch: &str,
        step: &mut Step,
    ) -> anyhow::Result<()> {
        self.check_branch_name(branch, &step.token()).await?;
        let initializing = "Initializing Git repository";
        step.step(Labels::new(initializing, initializing), async |step| {
            let creating = "Creating Git repository";
            step.step(Labels::new(creating, creating), async |step| {
                // `-b`: HEAD on `branch`, whatever `init.defaultBranch`
                // says (difference 12; <https://git-scm.com/docs/git-init#Documentation/git-init.txt--bltbranch-namegt>).
                let init = self.git().arg("init").arg("-b").shown_value(branch);
                self.change(step, init)
                    .await
                    .with_context(|| ran(&format!("init -b {branch}")))
            })
            .await?;
            let configuring = "Configuring Git remote";
            step.step(Labels::new(configuring, configuring), async |step| {
                let add = self
                    .git()
                    .arg("remote")
                    .arg("add")
                    .arg("origin")
                    .shown_value(url);
                self.change(step, add)
                    .await
                    .with_context(|| ran(&format!("remote add origin {url}")))
            })
            .await?;
            let fetching = "Fetching repository branches";
            step.step(Labels::new(fetching, fetching), async |step| {
                self.remote(step, &["fetch", "--progress", "--all", "--prune"])
                    .await
                    .context(ran("fetch --progress --all --prune"))
            })
            .await?;
            let tracking = format!("origin/{branch}");
            let creating = format!("Creating branch {branch}");
            step.step(Labels::new(&creating, &creating), async |step| {
                let create = self
                    .git()
                    .arg("branch")
                    .shown_value(branch)
                    .shown_value(&tracking);
                self.change(step, create)
                    .await
                    .with_context(|| ran(&format!("branch {branch} {tracking}")))
            })
            .await?;
            let resetting = format!("Resetting to branch {branch}");
            step.step(Labels::new(&resetting, &resetting), async |step| {
                let reset = self.git().arg("reset").arg("--hard").shown_value(&tracking);
                self.change(step, reset)
                    .await
                    .with_context(|| ran(&format!("reset --hard {tracking}")))
            })
            .await?;
            self.submodules(step, &ran("submodule update --progress --init --recursive"))
                .await
        })
        .await?;
        Ok(())
    }

    /// "Updating git submodules": `git submodule update --progress --init
    /// --recursive`, a remote operation, its failure under `failure`
    /// inside the step (a step's caller gets only its exit).
    async fn submodules(&self, step: &mut Step, failure: &str) -> anyhow::Result<()> {
        let submodules = "Updating git submodules";
        step.step(Labels::new(submodules, submodules), async |step| {
            self.remote(
                step,
                &["submodule", "update", "--progress", "--init", "--recursive"],
            )
            .await
            .context(failure.to_owned())
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
            .shown_value(format!("refs/remotes/origin/{branch}"));
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

    /// `branch`'s upstream, before anything changes (repo spec, "Order"):
    /// a local branch, and the current one always, must name one (`git
    /// rev-parse --verify --quiet <branch>@{u}`); a branch only on the
    /// remote needs none, since the reset's `checkout <branch> --` creates
    /// it tracking `origin/<branch>` (git's guess,
    /// <https://git-scm.com/docs/git-checkout#Documentation/git-checkout.txt-gitcheckoutbranch>).
    async fn require_upstream(
        &self,
        branch: &str,
        current: bool,
        token: &CancellationToken,
    ) -> Result<(), RepoError> {
        // A ref that isn't there exits 1; 128 is git failing, an error.
        if !current
            && !self
                .resolves(format!("refs/heads/{branch}"), &[1], token)
                .await?
        {
            return Ok(());
        }
        if self
            .resolves(format!("{branch}@{{u}}"), UNRESOLVED, token)
            .await?
        {
            Ok(())
        } else {
            Err(RepoError::NoUpstream {
                repo: self.name.clone(),
                branch: branch.to_string(),
            })
        }
    }

    /// Whether `rev` names a commit: `git rev-parse --verify --quiet
    /// <rev>` exits 0, or one of the codes in `missing`, which mean it
    /// doesn't; any other ending is git's error.
    pub(crate) async fn resolves(
        &self,
        rev: String,
        missing: &[i32],
        token: &CancellationToken,
    ) -> Result<bool, RepoError> {
        let cmd = self
            .git()
            .arg("rev-parse")
            .arg("--verify")
            .arg("--quiet")
            .shown_value(rev);
        let ran = self.read(cmd, token).await?;
        match ran.outcome.ending {
            Ending::Exited(0) => Ok(true),
            Ending::Exited(code) if missing.contains(&code) => Ok(false),
            // Interrupted, timed out, killed or another exit: exec's own
            // text.
            _ => Err(RepoError::Git(
                ran.check().err().map_or_default(|f| failure(&f)),
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
            .shown_value(branch)
            .arg("--");
        self.change(step, checkout).await?;
        self.change(step, clean()).await?;
        // Checked before anything changed too ([`Repo::require_upstream`]);
        // here again for the branch now checked out, which git's checkout
        // guess may have left without one (root's `branch.autoSetupMerge`
        // off; repo spec, Known limits).
        if !self
            .resolves("@{u}".to_owned(), UNRESOLVED, &step.token())
            .await?
        {
            return Err(RepoError::NoUpstream {
                repo: self.name.clone(),
                branch: branch.to_string(),
            }
            .into());
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

/// sb-go's text for a failed init command (saltbox/setup.go:276):
/// `error running command [git <args>]`.
fn ran(args: &str) -> String {
    format!("error running command [git {args}]")
}
