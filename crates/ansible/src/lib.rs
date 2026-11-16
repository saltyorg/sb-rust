//! `sb-ansible`: Saltbox's playbooks run by tag (`sb install`, `sb diag`),
//! the tag listing and its cache (ansible spec). Every child is
//! `ansible-playbook` through sb-exec: `attached` for the runs a user
//! watches, so Ansible's prompts reach the terminal (spec, assumption 2).
//! So far the first five slices: the playbook paths ([`Repo`],
//! [`Repos`]), one attached run by tag ([`Ansible::run`],
//! [`pass_through`]), `sb install`'s arguments, tags and routing
//! ([`InstallArgs`], [`parse_tags`], [`Routed`]), and the tag cache
//! ([`TagCache`]: `load`, `tags`, `refresh`; [`Ansible::list_tags`]),
//! install's validation ([`known_tags`], [`validate`],
//! [`render_suggestions`]), and `sb list` ([`gather`], [`render_list`],
//! [`render_search`]).

// sb's lines go to the writer a caller passes; `print!`/`eprint!` panic on a
// closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod args;
mod cache;
mod list;
mod run;
mod suggest;

pub use args::{InstallArgs, NoTags, Routed, parse_tags};
pub use cache::{
    CacheError, FILE as CACHE_FILE, LazyRunner, Listed, RepoTags, TagCache, label as cache_label,
};
pub use list::{
    ListArgs, Listing, Match, PIPED_WIDTH, Section, failures_error as list_failures, gather,
    render_list, render_search, search,
};
pub use run::{RunArgs, RunError, Tags, pass_through};
pub use suggest::{
    Hint, Known, SaltboxBroken, Side, Suggestion, known_tags, render as render_suggestions,
    validate,
};

use std::path::{Path, PathBuf};

/// Where Saltbox installs `ansible-playbook` (sb-go layout/layout.go):
/// run by its absolute path, never looked up in `PATH`.
pub const PROGRAM: &str = "/usr/local/bin/ansible-playbook";

/// Which of the three playbook repositories (spec, assumption 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Saltbox, at the Saltbox root.
    Saltbox,
    /// Sandbox, at `<server_appdata_path>/sandbox`.
    Sandbox,
    /// `saltbox_mod`, at `<server_appdata_path>/saltbox_mod`.
    Mod,
}

impl Kind {
    /// Its playbook's file name, in the repository's directory.
    #[must_use]
    pub fn playbook(self) -> &'static str {
        match self {
            Self::Saltbox => "saltbox.yml",
            Self::Sandbox => "sandbox.yml",
            Self::Mod => "saltbox_mod.yml",
        }
    }
}

/// A playbook repository: which one, and its directory, where its
/// playbook runs so the repository's own `ansible.cfg` applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    kind: Kind,
    dir: PathBuf,
}

impl Repo {
    /// Saltbox, at the Saltbox `root` (`/srv/git/saltbox`, or
    /// `SB_SALTBOX_DIR`'s, cli-core's `saltbox_dir`).
    #[must_use]
    pub fn saltbox(root: &Path) -> Self {
        Self {
            kind: Kind::Saltbox,
            dir: root.to_path_buf(),
        }
    }

    /// Sandbox, under the `appdata` root (`server_appdata_path`, cli-core's
    /// inventory read).
    #[must_use]
    pub fn sandbox(appdata: &Path) -> Self {
        Self {
            kind: Kind::Sandbox,
            dir: appdata.join("sandbox"),
        }
    }

    /// `saltbox_mod`, under the `appdata` root.
    #[must_use]
    pub fn saltbox_mod(appdata: &Path) -> Self {
        Self {
            kind: Kind::Mod,
            dir: appdata.join("saltbox_mod"),
        }
    }

    /// Which repository it is.
    #[must_use]
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Its directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Its playbook's absolute path (sb-go's success line names it).
    #[must_use]
    pub fn playbook_path(&self) -> PathBuf {
        self.dir.join(self.kind.playbook())
    }
}

/// The three playbook repositories of one host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repos {
    /// At the Saltbox root.
    pub saltbox: Repo,
    /// At `<appdata>/sandbox`.
    pub sandbox: Repo,
    /// At `<appdata>/saltbox_mod`.
    pub saltbox_mod: Repo,
}

impl Repos {
    /// Saltbox at the Saltbox `root`, Sandbox and `saltbox_mod` under the
    /// `appdata` root (spec, assumption 3).
    #[must_use]
    pub fn new(root: &Path, appdata: &Path) -> Self {
        Self {
            saltbox: Repo::saltbox(root),
            sandbox: Repo::sandbox(appdata),
            saltbox_mod: Repo::saltbox_mod(appdata),
        }
    }
}

/// `ansible-playbook`: [`PROGRAM`], or another program in its place (tests
/// and debugging: the binary's `SB_ANSIBLE_PLAYBOOK`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ansible {
    program: PathBuf,
}

impl Default for Ansible {
    fn default() -> Self {
        Self::with_program(PROGRAM)
    }
}

impl Ansible {
    /// `program` in place of [`PROGRAM`].
    #[must_use]
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The program it runs.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sb-go's layout: Saltbox at its root, Sandbox and `saltbox_mod` under
    /// the appdata root, each with its own playbook.
    #[test]
    fn each_repo_has_its_directory_and_playbook() {
        let saltbox = Repo::saltbox(Path::new("/srv/git/saltbox"));
        assert_eq!(saltbox.kind(), Kind::Saltbox);
        assert_eq!(saltbox.dir(), Path::new("/srv/git/saltbox"));
        assert_eq!(
            saltbox.playbook_path(),
            Path::new("/srv/git/saltbox/saltbox.yml")
        );
        let sandbox = Repo::sandbox(Path::new("/opt"));
        assert_eq!(sandbox.kind(), Kind::Sandbox);
        assert_eq!(
            sandbox.playbook_path(),
            Path::new("/opt/sandbox/sandbox.yml")
        );
        let modded = Repo::saltbox_mod(Path::new("/opt"));
        assert_eq!(modded.kind(), Kind::Mod);
        assert_eq!(
            modded.playbook_path(),
            Path::new("/opt/saltbox_mod/saltbox_mod.yml")
        );
    }

    #[test]
    fn repos_are_found_under_the_root_and_appdata() {
        let repos = Repos::new(Path::new("/srv/git/saltbox"), Path::new("/data"));
        assert_eq!(repos.saltbox, Repo::saltbox(Path::new("/srv/git/saltbox")));
        assert_eq!(repos.sandbox.dir(), Path::new("/data/sandbox"));
        assert_eq!(repos.saltbox_mod.dir(), Path::new("/data/saltbox_mod"));
    }

    #[test]
    fn the_program_is_saltboxs_unless_replaced() {
        assert_eq!(Ansible::default().program(), Path::new(PROGRAM));
        assert_eq!(
            Ansible::with_program("/x/fake").program(),
            Path::new("/x/fake")
        );
    }
}
