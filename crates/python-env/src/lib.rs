//! `sb-python-env`: the uv-managed Python, the Ansible venv Saltbox runs,
//! the 22.04 deadsnakes cleanup and `saltbox.fact` (python-env spec). So
//! far the first half of slice 1: the fixed [`Layout`], the toolchain pin
//! files with the uv minimum and the lock digest ([`toolchain`]), the
//! isolated child builder (`isolate`), the uv `--version` probe ([`uv`]),
//! and the Python and environment checks ([`venv`]).
//!
//! Every child is built as an sb-exec `Cmd` (spec, assumption 4); nothing
//! here draws.

// Output goes through steps; `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod isolate;
#[cfg(feature = "testing")]
pub mod testing;
pub mod toolchain;
pub mod uv;
pub mod venv;

pub use isolate::IsolateError;
pub use toolchain::{Toolchain, ToolchainError};
pub use uv::UvError;
pub use venv::CheckError;

use std::path::{Path, PathBuf};

use sb_exec::Cmd;
use tokio_util::sync::CancellationToken;

/// Every path python-env reads or writes (spec, "Fixed layout"; sb-go's
/// `layout.go:9-35`; decision 7): one field each, so a test builds the
/// whole tree on a temporary root with [`Layout::under`] (sb-go's
/// `releaseLayout` injection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The Saltbox checkout, `/srv/git/saltbox`.
    pub checkout: PathBuf,
    /// Its parent, `/srv/git`, given to the Saltbox user itself.
    pub git_dir: PathBuf,
    /// `<checkout>/.python-version`: the exact Python Saltbox runs.
    pub python_pin: PathBuf,
    /// `<checkout>/.uv-version`: the oldest uv Saltbox accepts.
    pub uv_pin: PathBuf,
    /// `<checkout>/requirements/requirements-saltbox.txt`: the hashed lock.
    pub lock: PathBuf,
    /// `/srv/ansible`.
    pub ansible: PathBuf,
    /// `/srv/ansible/venv`: the active venv, a symlink to a generation's.
    pub active_venv: PathBuf,
    /// `/srv/ansible/releases`: the generations.
    pub ansible_releases: PathBuf,
    /// `/srv/python`, which Saltbox's own roles install into too.
    pub python: PathBuf,
    /// `/srv/python/releases`: the Python releases sb manages.
    pub python_releases: PathBuf,
    /// `/usr/local/bin/uv`.
    pub uv: PathBuf,
    /// `/usr/local/bin/uvx`.
    pub uvx: PathBuf,
    /// `/var/cache/sb-go/uv`: uv's cache, shared with sb-go.
    pub uv_cache: PathBuf,
    /// `/usr/local/bin`: the wrappers of the venv's commands.
    pub wrappers: PathBuf,
    /// `<checkout>/ansible_facts.d/saltbox.fact`.
    pub fact: PathBuf,
    /// `/tmp`: where each child's scratch `TMPDIR` is made (sb-go's
    /// `scratchParent`).
    pub scratch: PathBuf,
}

impl Layout {
    /// sb-go's paths.
    #[must_use]
    pub fn saltbox() -> Self {
        Self::under(Path::new("/"))
    }

    /// sb-go's paths, each under `root` in place of `/` (tests; `/` gives
    /// [`Layout::saltbox`]).
    #[must_use]
    pub fn under(root: &Path) -> Self {
        let at = |path: &str| root.join(path);
        let checkout = at("srv/git/saltbox");
        let ansible = at("srv/ansible");
        let python = at("srv/python");
        let bin = at("usr/local/bin");
        Self {
            python_pin: checkout.join(".python-version"),
            uv_pin: checkout.join(".uv-version"),
            lock: checkout.join("requirements/requirements-saltbox.txt"),
            fact: checkout.join("ansible_facts.d/saltbox.fact"),
            checkout,
            git_dir: at("srv/git"),
            active_venv: ansible.join("venv"),
            ansible_releases: ansible.join("releases"),
            ansible,
            python_releases: python.join("releases"),
            python,
            uv: bin.join("uv"),
            uvx: bin.join("uvx"),
            wrappers: bin,
            uv_cache: at("var/cache/sb-go/uv"),
            scratch: at("tmp"),
        }
    }
}

/// What python-env's calls run with: the layout, the uv this sb pins
/// (`cli-core::UV_VERSION`, passed in by `sb`) and the programs it runs
/// besides the layout's.
#[derive(Debug, Clone)]
pub struct Env<'a> {
    pub layout: &'a Layout,
    /// The uv release this sb provides.
    pub uv_version: &'a str,
    /// git, for the checkout's commit (`git` from `PATH`).
    pub git: PathBuf,
}

impl<'a> Env<'a> {
    /// `layout` with `uv_version`, git from `PATH`.
    #[must_use]
    pub fn new(layout: &'a Layout, uv_version: &'a str) -> Self {
        Self {
            layout,
            uv_version,
            git: PathBuf::from("git"),
        }
    }
}

/// `cmd` captured to its end: its stdout and stderr read as one (sb-go
/// reads the combined output) when it exits 0, else exec's text (the
/// spawn error and its cause, or the command, its ending and stderr's last
/// line). The caller sets its timeout.
pub(crate) async fn captured(cmd: &Cmd, token: &CancellationToken) -> Result<String, String> {
    let ran = cmd
        .capture(token)
        .await
        .map_err(|err| format!("{err}: {}", err.cause()))?;
    let ran = ran
        .check()
        .map_err(|failed| sb_host::child_failure(&failed))?;
    Ok(format!("{}{}", ran.stdout, ran.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests that write a stand-in program and run it hold this from
    /// writing it until its run is over, so no other test's fork holds its
    /// write descriptor when it runs (exec fails with ETXTBSY otherwise).
    pub(crate) static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// sb-go's layout.go, path for path.
    #[test]
    fn the_layout_is_sb_gos() {
        let layout = Layout::saltbox();
        let paths = [
            (&layout.checkout, "/srv/git/saltbox"),
            (&layout.git_dir, "/srv/git"),
            (&layout.python_pin, "/srv/git/saltbox/.python-version"),
            (&layout.uv_pin, "/srv/git/saltbox/.uv-version"),
            (
                &layout.lock,
                "/srv/git/saltbox/requirements/requirements-saltbox.txt",
            ),
            (&layout.ansible, "/srv/ansible"),
            (&layout.active_venv, "/srv/ansible/venv"),
            (&layout.ansible_releases, "/srv/ansible/releases"),
            (&layout.python, "/srv/python"),
            (&layout.python_releases, "/srv/python/releases"),
            (&layout.uv, "/usr/local/bin/uv"),
            (&layout.uvx, "/usr/local/bin/uvx"),
            (&layout.uv_cache, "/var/cache/sb-go/uv"),
            (&layout.wrappers, "/usr/local/bin"),
            (
                &layout.fact,
                "/srv/git/saltbox/ansible_facts.d/saltbox.fact",
            ),
            (&layout.scratch, "/tmp"),
        ];
        for (path, want) in paths {
            assert_eq!(path, Path::new(want));
        }
    }

    #[test]
    fn a_test_layout_lives_under_its_root() {
        let layout = Layout::under(Path::new("/scratch/root"));
        assert_eq!(
            layout.active_venv,
            Path::new("/scratch/root/srv/ansible/venv")
        );
        assert_eq!(layout.wrappers, Path::new("/scratch/root/usr/local/bin"));
    }
}
