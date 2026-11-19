//! `sb-python-env`: the uv-managed Python, the Ansible venv Saltbox runs,
//! the 22.04 deadsnakes cleanup and `saltbox.fact` (python-env spec). So
//! far slices 1 to 6: the fixed [`Layout`], the toolchain
//! ([`toolchain`]), the isolated child builder (`isolate`), the uv probe
//! and install ([`uv`]), `saltbox.fact` ([`fact`]), the 22.04 deadsnakes
//! cleanup ([`deadsnakes`]), the venv's checks,
//! its reconcile with the clean retry and the build into staging
//! ([`venv`]), the manifest, the
//! wrappers, activation and the generation cleanup ([`activate`]), and
//! [`reconcile_runtime`].
//!
//! Every child is built as an sb-exec `Cmd` (spec, assumption 4); nothing
//! here draws.

// Output goes through steps; `print!`/`eprint!` panic on a closed pipe.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod activate;
pub mod deadsnakes;
pub mod fact;
mod isolate;
pub mod manifest;
#[cfg(feature = "testing")]
pub mod testing;
pub mod toolchain;
pub mod uv;
pub mod venv;
pub mod verify;
pub mod wrappers;

pub use deadsnakes::{DeadsnakesError, RepositoriesError, cleanup_deadsnakes};
pub use fact::{FactError, FactMode, FactOutcome, install_fact};
pub use isolate::IsolateError;
pub use toolchain::{Toolchain, ToolchainError};
pub use uv::{ExtractError, UvError};
pub use venv::{CheckError, Force, VenvError, VenvOutcome, reconcile_venv};

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sb_cli_core::exit::Exit;
use sb_exec::Cmd;
use sb_host::apt::Apt;
use sb_term_ui::{Labels, Step};
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
    /// `/var/lib/sb/python-releases`: each release's file manifest, which
    /// sb writes when it installs the release (decision 25), outside
    /// `/srv/python`, which step 16 gives to the Saltbox user.
    pub python_manifests: PathBuf,
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

    /// The layout `sb` runs on: [`Layout::saltbox`]; in a build with the
    /// `testing` feature (the sb `cargo test` runs), the stand-in tree
    /// under [`testing::ROOT_VAR`] when that is set.
    #[must_use]
    pub fn for_sb() -> Self {
        #[cfg(feature = "testing")]
        if let Some(root) = testing::root() {
            return Self::under(&root);
        }
        Self::saltbox()
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
            python_manifests: at("var/lib/sb/python-releases"),
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
/// (`cli-core::UV_VERSION`, passed in by `sb`), the release client and
/// the build's architecture its downloads are for (spec, "Library
/// surface"), and the programs it runs besides the layout's.
#[derive(Debug, Clone)]
pub struct Env<'a> {
    pub layout: &'a Layout,
    /// The release client: uv's and the fact's metadata and downloads.
    pub release: &'a sb_release::Releases,
    /// The uv release this sb provides.
    pub uv_version: &'a str,
    /// Which assets to download (decision 8).
    pub arch: sb_release::Arch,
    /// git, for the checkout's commit (`git` from `PATH`).
    pub git: PathBuf,
    /// apt and dpkg, for libpq-dev (spec, step 3) and the deadsnakes
    /// cleanup.
    pub apt: Apt,
    /// The Ubuntu release (os-release's `VERSION_ID`) the deadsnakes
    /// cleanup takes the host for; `None` reads the host's
    /// (`sb_host::ubuntu_release`).
    pub ubuntu_release: Option<String>,
    /// The run id naming the deadsnakes backup directory: the run log's
    /// (cli-core's `RunLog::id()`, which `sb` sets). Without a run log,
    /// the run's start in UTC, `YYYYmmddTHHMMSSnnnnnnnnn`, letters and
    /// digits as a run id must be.
    pub run_id: String,
    /// `sb update --verify-venv` (decision 25): the active venv's files
    /// checked against their wheels' RECORDs and its Python release's
    /// against sb's manifest; off for every other run.
    pub verify: bool,
}

impl<'a> Env<'a> {
    /// `layout` with `release`, `uv_version` and `arch`, git, apt and
    /// dpkg from `PATH`.
    #[must_use]
    pub fn new(
        layout: &'a Layout,
        release: &'a sb_release::Releases,
        uv_version: &'a str,
        arch: sb_release::Arch,
    ) -> Self {
        Self {
            layout,
            release,
            uv_version,
            arch,
            git: PathBuf::from("git"),
            apt: Apt::new(),
            ubuntu_release: None,
            // `%N` is `%9f`, nine digits (jiff's strtime docs:
            // <https://docs.rs/jiff/0.2.38/jiff/fmt/strtime/index.html>).
            run_id: jiff::Timestamp::now()
                .strftime("%Y%m%dT%H%M%S%N")
                .to_string(),
            verify: false,
        }
    }

    /// The `Env` `sb` runs with: [`Env::new`]; in a build with the
    /// `testing` feature, the stand-in tree's git, dpkg-query and apt-get
    /// when [`testing::ROOT_VAR`] is set.
    #[must_use]
    pub fn for_sb(
        layout: &'a Layout,
        release: &'a sb_release::Releases,
        uv_version: &'a str,
        arch: sb_release::Arch,
    ) -> Self {
        let env = Self::new(layout, release, uv_version, arch);
        #[cfg(feature = "testing")]
        if let Some(root) = testing::root() {
            return testing::with_stand_ins(env, &root);
        }
        env
    }
}

/// Why the Saltbox runtime's update stopped: one of its steps failed or
/// was interrupted, and says why itself.
#[derive(Debug, thiserror::Error)]
#[error("a step stopped the Saltbox runtime's update")]
pub struct RuntimeError(#[from] Exit);

/// The Saltbox runtime brought in line (spec, `reconcile_runtime`; sb-go's
/// `reconcileSaltboxRuntimeWith`, cmd/update.go:204-249), shared by `sb
/// update` and `sb update-venv`: three nested steps in sb-go's order and
/// words, the old deadsnakes packages, the venv ([`reconcile_venv`] with
/// no force) and `saltbox.fact`.
///
/// # Errors
///
/// [`RuntimeError`] when a step fails; the step shows sb-go's error.
pub async fn reconcile_runtime(
    step: &mut Step,
    env: &Env<'_>,
    user: Option<&str>,
) -> Result<(), RuntimeError> {
    let deadsnakes = "Checking for old deadsnakes Python packages";
    step.step(Labels::new(deadsnakes, deadsnakes), async |step| {
        let cleaned = cleanup_deadsnakes(step, env)
            .await
            .context("error cleaning up deadsnakes packages")?;
        if cleaned {
            step.info("Removed old deadsnakes Python packages");
        }
        anyhow::Ok(())
    })
    .await?;
    let venv = Labels::new(
        "Preparing Ansible virtual environment",
        "Ansible virtual environment ready",
    )
    .failure("Ansible virtual environment");
    step.step(venv, async |step| {
        reconcile_venv(step, env, user, Force::None)
            .await
            .context("error managing Ansible venv")?;
        anyhow::Ok(())
    })
    .await?;
    let fact = Labels::new("Checking saltbox.fact", "saltbox.fact is ready")
        .failure("saltbox.fact update");
    step.step(fact, async |step| {
        install_fact(step, env, FactMode::Update)
            .await
            .context("error downloading and installing saltbox fact")?;
        anyhow::Ok(())
    })
    .await?;
    Ok(())
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
            (&layout.python_manifests, "/var/lib/sb/python-releases"),
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
