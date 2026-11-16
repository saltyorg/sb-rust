//! `sb gha`, hidden (ops spec, "`sb gha` (hidden)"; sb-go's cmd/gha.go):
//! what Saltbox's CI needs before its role jobs, in sb-go's order under
//! one root step: the prerequisites, the locale, the venv with no Saltbox
//! user, saltbox.fact, the default configs, those configs given to the
//! user who ran sudo, and the git hooks. No release, architecture, LXC or
//! desktop check, no clone, no guard, no flags. The Saltbox root may be a
//! symlink (the CI links its workspace there); nothing here refuses one.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sb_cli_core::exit::Exit;
use sb_host::apt::Apt;
use sb_python_env::{Env, FactMode, Force, install_fact, reconcile_venv};
use sb_term_ui::{Labels, Runner};

use crate::locale::Locale;
use crate::prereq::Dirs;
use crate::{configs, hooks, locale, prereq};

/// The config files gha gives to the user who ran sudo, in the Saltbox
/// root (sb-go's `layout.Saltbox*ConfigPath`, in its order).
pub const CONFIGS: [&str; 6] = [
    "accounts.yml",
    "adv_settings.yml",
    "backup_config.yml",
    "hetzner_vlan.yml",
    "providers.yml",
    "settings.yml",
];

/// What gha runs with.
#[derive(Debug)]
pub struct Gha<'a> {
    /// host's apt, for the prerequisites.
    pub apt: &'a Apt,
    /// `/srv/git` and `/srv/ansible`.
    pub dirs: &'a Dirs,
    /// The locale programs and file.
    pub locale: &'a Locale,
    /// The program the git hooks run with.
    pub bash: &'a Path,
    /// The Saltbox root (`/srv/git/saltbox`, a symlink under the CI).
    pub root: &'a Path,
    /// python-env's layout, release client and programs.
    pub python: &'a Env<'a>,
    /// The run log's id, naming the apt sources' backup.
    pub run_id: &'a str,
    /// `SUDO_USER`, as given; trimmed here, and empty or unknown skips the
    /// ownership (host's `ensure_owner`).
    pub sudo_user: Option<&'a str>,
}

impl Gha<'_> {
    /// The six config files' paths in the root.
    #[must_use]
    pub fn configs(&self) -> Vec<PathBuf> {
        CONFIGS.iter().map(|name| self.root.join(name)).collect()
    }
}

/// gha under one root step `Installing GHA dependencies` (success `GHA
/// dependencies installed`), each part's error in sb-go's words around
/// the part's own.
///
/// # Errors
///
/// The step's quiet `Exit` when a part failed (its block shown under
/// it).
pub async fn run(runner: &mut Runner, gha: &Gha<'_>) -> Result<(), Exit> {
    let labels = Labels::new("Installing GHA dependencies", "GHA dependencies installed");
    runner
        .step(labels, async |step| {
            prereq::prerequisites(step, gha.apt, gha.dirs, gha.run_id)
                .await
                .context("error during initial setup")?;
            locale::configure(step, gha.locale)
                .await
                .context("error configuring locale")?;
            reconcile_venv(step, gha.python, None, Force::None)
                .await
                .context("error reconciling Python environment")?;
            let fact = Labels::new("Checking saltbox.fact", "saltbox.fact is ready")
                .failure("saltbox.fact update");
            step.step(fact, async |step| {
                install_fact(step, gha.python, FactMode::Update).await?;
                Ok(())
            })
            .await
            .context("error downloading and installing saltbox.fact")?;
            configs::copy_default_configs(step, gha.root)
                .await
                .context("error copying default configuration files")?;
            let owner = gha.sudo_user.unwrap_or_default().trim().to_owned();
            let paths = gha.configs();
            // The user lookup blocks (NSS): on the blocking pool, as
            // sb-host asks, with the run's span (cli-core's `spawn`).
            sb_cli_core::spawn::blocking(move || sb_host::ensure_owner(&owner, &paths))
                .await?
                .context("error assigning GHA configuration ownership")?;
            hooks::git_hooks(step, gha.root, gha.bash)
                .await
                .context("error initializing Git hooks")?;
            Ok(())
        })
        .await
}
