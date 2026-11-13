//! `sb self-update`'s flow (release spec, "`sb self-update`"), sb-go's
//! `selfupdate.Run` with the spec's decisions: off in builds with
//! self-update disabled and in raw builds unless forced; the newest
//! stable release looked up; the user asked on the terminal (or `--yes`);
//! the asset downloaded and verified next to the target, then put in its
//! place. Runner steps show it: one runner for the lookup, the question
//! with no runner drawing (a prompt can't share the screen with live
//! lines), then one for the update.

use std::path::Path;

use anyhow::Context as _;
use sb_cli_core::build_info::BuildInfo;
use sb_term_ui::{Labels, Runner};
use semver::Version;

use crate::{GithubRepo, Release, Releases, Stage, activate};

/// The question, as sb-go asks it.
const QUESTION: &str = "Do you want to update";
/// The staged file's name before it replaces sb (sb-go's pattern).
const PREFIX: &str = ".sb-update-";
/// The largest sb binary accepted (sb-go's cap).
const MAX_SIZE: u64 = 128 << 20;

/// What a self-update is asked to do.
#[derive(Clone, Copy, Debug)]
pub struct SelfUpdate<'a> {
    /// The repository sb's releases come from.
    pub repo: GithubRepo,
    /// The running build.
    pub build: &'a BuildInfo,
    /// The file to replace: sb's own executable (a copy in tests).
    pub target: &'a Path,
    /// The answer given (`--yes`, or `sb update`'s automatic run).
    pub yes: bool,
    /// `--force-update`: run although this build has self-update off.
    pub force: bool,
    /// Shown as a warning after a successful update (`sb update`'s).
    pub message: Option<&'a str>,
}

/// How a self-update ended without an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Self-update is off in this build (the step says so).
    Disabled,
    /// The running version is the newest.
    Latest,
    /// The user said no.
    Declined,
    /// sb was replaced with this version.
    Updated(Version),
}

/// The asset for this build's architecture (release spec, decision 9).
///
/// # Errors
///
/// An architecture with no sb release.
pub fn asset_name() -> anyhow::Result<&'static str> {
    asset_for(std::env::consts::ARCH)
}

fn asset_for(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "x86_64" => Ok("sb_linux_amd64"),
        "aarch64" => Ok("sb_linux_arm64"),
        other => anyhow::bail!("unsupported architecture {other}"),
    }
}

/// Run a self-update: each runner from `runner` (titled and logged by the
/// caller), the question in between.
///
/// # Errors
///
/// The step's [`Exit`](sb_cli_core::exit::Exit) when a step failed (its
/// error shown in the tree: `check for updates: ...`, `binary update
/// failed: ...`), or the prompt's error (no terminal and no `--yes`).
pub async fn self_update(
    releases: &Releases,
    update: SelfUpdate<'_>,
    mut runner: impl FnMut() -> Runner,
) -> anyhow::Result<Outcome> {
    let build = update.build;
    // A raw build counts as disabled (decision 11): a developer build
    // never replaces itself with a release.
    let disabled = build.self_update_off();
    let mut checking = runner();
    let found = checking
        .step(
            Labels::new("Checking for sb CLI updates", "Checked for sb CLI updates"),
            async |step| {
                if disabled && !update.force {
                    step.warn("Self-update is disabled in this build");
                    step.info("Use --force-update to override this restriction");
                    return Ok(None);
                }
                if disabled {
                    step.info("Forcing self-update despite build configuration");
                }
                let current = Version::parse(&build.version).with_context(|| {
                    format!(
                        "check for updates: invalid current version {:?}",
                        build.version
                    )
                })?;
                let asset = asset_name().context("check for updates")?;
                let Some(newest) = releases
                    .newest(update.repo, &current, asset, step)
                    .await
                    .context("check for updates")?
                else {
                    step.info(format!(
                        "Current binary is the latest version: {}",
                        build.version
                    ));
                    return Ok(None);
                };
                // `newest` picks among releases with a version only.
                let version = newest.version.clone().with_context(|| {
                    format!("check for updates: release {} has no version", newest.tag)
                })?;
                step.info(format!(
                    "New sb CLI version available: {version} (current: {current})"
                ));
                Ok(Some((newest, version)))
            },
        )
        .await;
    checking.finish();
    let Some((newest, version)) = found? else {
        return Ok(if disabled && !update.force {
            Outcome::Disabled
        } else {
            Outcome::Latest
        });
    };
    if !sb_term_ui::confirm(QUESTION, update.yes.then_some(true), "--yes")? {
        return Ok(Outcome::Declined);
    }
    install(releases, update, &newest, &version, runner()).await?;
    Ok(Outcome::Updated(version))
}

/// Download `newest`'s asset next to the target and put it in place, on
/// `updating`'s steps.
async fn install(
    releases: &Releases,
    update: SelfUpdate<'_>,
    newest: &Release,
    version: &Version,
    mut updating: Runner,
) -> anyhow::Result<()> {
    let done = updating
        .step(
            Labels::new(
                format!("Updating sb CLI to {version}"),
                format!("Updated sb CLI to {version}"),
            ),
            async |step| {
                let dir = update
                    .target
                    .parent()
                    .context("binary update failed: sb's path has no directory")?;
                let asset = &newest.asset;
                let staged = step
                    .step(
                        Labels::new(
                            format!("Downloading {}", asset.name()),
                            format!("Downloaded {}", asset.name()),
                        ),
                        async |step| {
                            let stage = Stage {
                                dir,
                                prefix: PREFIX,
                                max_size: MAX_SIZE,
                            };
                            releases
                                .download(asset, stage, step)
                                .await
                                .context("binary update failed")
                        },
                    )
                    .await?;
                step.step(Labels::new("Installing", "Installed"), async |_| {
                    activate(staged, update.target, 0o755).context("binary update failed")
                })
                .await?;
                step.info(format!("Successfully updated sb CLI to version: {version}"));
                if let Some(message) = update.message {
                    step.warn(message);
                }
                Ok(())
            },
        )
        .await;
    updating.finish();
    Ok(done?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_asset_follows_the_architecture() {
        assert_eq!(asset_for("x86_64").expect("amd64"), "sb_linux_amd64");
        assert_eq!(asset_for("aarch64").expect("arm64"), "sb_linux_arm64");
        assert_eq!(
            asset_for("riscv64").expect_err("none").to_string(),
            "unsupported architecture riscv64"
        );
        assert!(asset_name().is_ok());
    }
}
