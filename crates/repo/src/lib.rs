//! `sb-repo`: git on Saltbox's checkouts, Saltbox (`/srv/git/saltbox`) and
//! Sandbox (`<server_appdata_path>/sandbox`) (repo spec). Every child is
//! the host's git through sb-exec (decision 1): captures for the local
//! reads, streams into term-ui steps for everything that changes the
//! checkout. Why not gix or git2 (decision 1): gix has no checkout, reset,
//! clean or submodule-update orchestration and only blocking HTTP; git2
//! is libgit2, C, with openssl for https. So far the first slice: [`Repo::open`],
//! [`Repo::current_branch`], [`Repo::check_branch_name`],
//! [`Repo::ensure_fetch_all_branches`] and [`Repo::fetch_and_reset`].

// Output goes through steps; `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod git;
mod update;

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
    /// A local git read failed otherwise.
    #[error("{0}")]
    Git(String),
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
