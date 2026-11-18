//! `sb-repo`: git on Saltbox's checkouts, Saltbox (`/srv/git/saltbox`) and
//! Sandbox (`<server_appdata_path>/sandbox`) (repo spec). Every child is
//! the host's git through sb-exec (decision 1): captures for the local
//! reads, streams into term-ui steps for everything that changes the
//! checkout. Why not gix or git2 (decision 1): gix has no checkout, reset,
//! clean or submodule-update orchestration and only blocking HTTP; git2
//! is libgit2, C, with openssl for https. So far the first slice: [`Repo::open`],
//! [`Repo::current_branch`], [`Repo::check_branch_name`],
//! [`Repo::ensure_fetch_all_branches`] and [`Repo::fetch_and_reset`], then
//! [`Repo::head`], [`branch_choice`] and [`branch_notices`], and setup's
//! [`Repo::clone`] and [`Repo::init_from_remote`].

// Output goes through steps; `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod git;
mod remote;
mod update;

pub use remote::PublicRepoError;

use std::io;
use std::path::{Path, PathBuf};

/// A checkout sb updates: its name for labels and errors ("Saltbox",
/// "Sandbox") and its path.
#[derive(Debug, Clone)]
pub struct Repo {
    name: String,
    path: PathBuf,
    git: PathBuf,
}

/// Why a repository operation failed, in sb-go's words where sb-go has
/// them.
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    /// The checkout's path couldn't be inspected.
    #[error("inspect required directory {}: {cause}", path.display())]
    Inspect { path: PathBuf, cause: io::Error },
    /// A clone's destination is already there (sb-go's `CloneRepository`,
    /// git/git.go:19-21).
    #[error("destination path '{}' already exists", .0.display())]
    Destination(PathBuf),
    /// The checkout's path isn't a directory.
    #[error("{} exists but is not a directory", .0.display())]
    NotDirectory(PathBuf),
    /// `git rev-parse --abbrev-ref HEAD` failed.
    #[error("failed to get current branch: {0}")]
    CurrentBranch(String),
    /// A branch name git refuses or rewrites, or one starting with `-`
    /// (repo spec, decision 3). The name is shown escaped.
    #[error("'{0}' is not a valid branch name")]
    BranchName(String),
    /// `git config --get-all remote.origin.fetch` failed.
    #[error("failed to read remote.origin.fetch: {0}")]
    ReadFetch(String),
    /// `git config --add remote.origin.fetch` failed.
    #[error("failed to update remote.origin.fetch: {0}")]
    UpdateFetch(String),
    /// The branch isn't on the remote (decision 8).
    #[error("branch '{branch}' doesn't exist on {repo}'s remote")]
    MissingBranch { branch: String, repo: String },
    /// The branch has no upstream to reset to.
    #[error("{repo}: branch '{branch}' has no upstream to update from")]
    NoUpstream { repo: String, branch: String },
    /// `git rev-parse HEAD` failed: an unborn branch, no repository, or
    /// no git (sb-go's `GetGitCommitHash`, git/git.go:216-228).
    #[error("error occurred while trying to get the git commit hash: {0}")]
    Head(String),
    /// A local git read failed otherwise.
    #[error("{0}")]
    Git(String),
    /// A ref git doesn't know as a commit, or one starting with `-`
    /// ([`Repo::file_at`]). The ref is shown escaped.
    #[error("'{reference}' isn't a commit in {}", path.display())]
    Ref { reference: String, path: PathBuf },
    /// A file at a ref that isn't text sb reads ([`Repo::file_at`]):
    /// over cli-core's 1 MiB, or not UTF-8.
    #[error("{0}")]
    Blob(String),
}

/// The user's answer to "reset to the wanted branch?", from a flag or the
/// command's prompt (repo spec, decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Reset to the wanted branch (`--reset-branch`, or yes).
    Reset,
    /// Update the current branch (`--keep-branch`, or no).
    Keep,
}

/// Which branch an update uses (repo spec, `branch_choice`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Update this branch, the wanted one.
    Use(String),
    /// Update this branch, the current one, as the user chose. On a
    /// detached HEAD it is `HEAD`, which [`Repo::check_branch_name`]
    /// refuses: the command decides what keeping a detached HEAD means.
    Keep(String),
    /// The branches differ and there's no answer: the command asks, or
    /// fails naming the flag.
    Ask,
}

/// Whether the update needs the user's answer (sb-go's
/// `ResolveUpdateBranch`, git/git.go:80-134): the wanted branch when
/// `current` is it, otherwise as `answer` says, or [`Choice::Ask`]. A
/// detached HEAD (`current` is `HEAD`) is another branch.
#[must_use]
pub fn branch_choice(current: &str, wanted: &str, answer: Option<Answer>) -> Choice {
    if current == wanted {
        return Choice::Use(wanted.to_owned());
    }
    match answer {
        Some(Answer::Reset) => Choice::Use(wanted.to_owned()),
        Some(Answer::Keep) => Choice::Keep(current.to_owned()),
        None => Choice::Ask,
    }
}

/// sb-go's info lines for the final `choice` (git/git.go:101 and 125).
///
/// `<Repo>: Currently on branch '<b>'` when the branches differ, then
/// `<Repo>: Updating the current branch '<b>'` when the current one is
/// kept. Call it once, for the choice made after any prompt, so the first
/// line isn't shown twice. The
/// names are as git gives them: git's ref rules refuse control
/// characters (git-check-ref-format(1)), and a step's notice shows any
/// other text through term-ui's sanitizing.
#[must_use]
pub fn branch_notices(repo: &Repo, current: &str, wanted: &str, choice: &Choice) -> Vec<String> {
    let mut notices = Vec::new();
    if current != wanted {
        notices.push(format!("{}: Currently on branch '{current}'", repo.name));
    }
    if let Choice::Keep(branch) = choice {
        notices.push(format!(
            "{}: Updating the current branch '{branch}'",
            repo.name
        ));
    }
    notices
}

impl Repo {
    /// The checkout at `path`, named `name` (sb-go's `requireDirectory`,
    /// cmd/update.go:391-400).
    ///
    /// # Errors
    ///
    /// [`RepoError::Inspect`] when `path` can't be inspected (missing
    /// included), [`RepoError::NotDirectory`] when it isn't a directory.
    pub fn open(name: impl Into<String>, path: impl Into<PathBuf>) -> Result<Self, RepoError> {
        let path = path.into();
        let metadata = std::fs::metadata(&path).map_err(|cause| RepoError::Inspect {
            path: path.clone(),
            cause,
        })?;
        if !metadata.is_dir() {
            return Err(RepoError::NotDirectory(path));
        }
        Ok(Self {
            name: name.into(),
            path,
            git: PathBuf::from("git"),
        })
    }

    /// Run `program` as git instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_git(mut self, program: impl Into<PathBuf>) -> Self {
        self.git = program.into();
        self
    }

    /// The name for labels and errors.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The checkout's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
