//! `sb self-update`'s flow (release spec, "`sb self-update`"), sb-go's
//! `selfupdate.Run` with the spec's decisions: off in builds with
//! self-update disabled and in raw builds unless forced; the newest
//! stable release looked up; the user asked on the terminal (or `--yes`);
//! the asset downloaded and verified next to the target, checked as a
//! binary of this architecture that reports the release's version
//! (decision 12), then put in its place. Runner steps show it: one runner
//! for the lookup, the question with no runner drawing (a prompt can't
//! share the screen with live lines), then one for the update.

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use anyhow::Context as _;
use sb_cli_core::build_info::BuildInfo;
use sb_exec::Cmd;
use sb_term_ui::{Labels, Runner, Step};
use semver::Version;

use crate::lookup::cut;
use crate::{Arch, ElfError, GithubRepo, Release, Releases, Stage, activate, check_elf};

/// The question, as sb-go asks it.
const QUESTION: &str = "Do you want to update";
/// The staged file's name before it replaces sb (sb-go's pattern).
const PREFIX: &str = ".sb-update-";
/// The largest sb binary accepted (sb-go's cap).
const MAX_SIZE: u64 = 128 << 20;
/// How long the new binary's `version --json` may run (release spec,
/// step 4).
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

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
    Ok(asset_for(Arch::current()?))
}

fn asset_for(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "sb_linux_amd64",
        Arch::Arm64 => "sb_linux_arm64",
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
                let arch = Arch::current().context("check for updates")?;
                let asset = asset_for(arch);
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
                Ok(Some(Found {
                    release: newest,
                    version,
                    arch,
                }))
            },
        )
        .await;
    checking.finish();
    let Some(found) = found? else {
        return Ok(if disabled && !update.force {
            Outcome::Disabled
        } else {
            Outcome::Latest
        });
    };
    if !sb_term_ui::confirm(QUESTION, update.yes.then_some(true), "--yes")? {
        return Ok(Outcome::Declined);
    }
    install(releases, update, &found, runner()).await?;
    Ok(Outcome::Updated(found.version))
}

/// The newer release the lookup found, its version, and the architecture
/// its asset was chosen for.
struct Found {
    release: Release,
    version: Version,
    arch: Arch,
}

/// Download the release's asset next to the target, check it as a binary
/// of the found architecture and version, and put it in place, on
/// `updating`'s steps.
async fn install(
    releases: &Releases,
    update: SelfUpdate<'_>,
    found: &Found,
    mut updating: Runner,
) -> anyhow::Result<()> {
    let Found {
        release: newest,
        version,
        arch,
    } = found;
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
                step.step(
                    Labels::new("Checking the new binary", "Checked the new binary"),
                    async |step| {
                        check_new(staged.path(), *arch, &newest.tag, step)
                            .await
                            .context("binary update failed validation")
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

/// The staged binary's checks before it replaces sb (decision 12,
/// decided.md D-Q5): an ELF for `arch`; then, made executable, its own
/// `version --json`, run with an empty environment and a timeout, must
/// report `tag` without its `v`.
async fn check_new(staged: &Path, arch: Arch, tag: &str, step: &mut Step) -> anyhow::Result<()> {
    check_elf(staged, arch).map_err(|err| anyhow::anyhow!(elf_text(err)))?;
    std::fs::set_permissions(staged, Permissions::from_mode(0o755))
        .context("set the new binary's permissions")?;
    let ran = Cmd::new(staged)
        .arg("version")
        .arg("--json")
        .env_clear()
        .timeout(VERSION_TIMEOUT)
        .capture(&step.token())
        .await?
        .check()?;
    reports(&ran.stdout, tag).map_err(anyhow::Error::msg)
}

/// sb-go's texts for a binary that isn't this architecture's
/// (release/asset.go:29-39), arm64 worded as sb-go words amd64.
fn elf_text(err: ElfError) -> String {
    match err {
        ElfError::NotElf(cause) => format!("open ELF executable: {cause}"),
        ElfError::WrongMachine(Arch::Amd64) => {
            "executable is not a 64-bit x86 ELF binary".to_owned()
        }
        ElfError::WrongMachine(Arch::Arm64) => {
            "executable is not a 64-bit arm64 ELF binary".to_owned()
        }
    }
}

/// Whether `stdout`, a `version --json`, reports `tag` without its `v`;
/// else why not, naming what it printed (made safe and cut).
fn reports(stdout: &str, tag: &str) -> Result<(), String> {
    /// The one field read; sb's others are ignored.
    #[derive(serde::Deserialize)]
    struct Reported {
        version: String,
    }
    let want = tag.strip_prefix('v').unwrap_or(tag);
    match serde_json::from_str::<Reported>(stdout) {
        Ok(got) if got.version == want => Ok(()),
        Ok(got) => Err(format!(
            "the new binary reports version {}, not {want}",
            cut(&got.version)
        )),
        Err(_) if stdout.trim().is_empty() => {
            Err("the new binary's version --json printed nothing".to_owned())
        }
        Err(_) => Err(format!(
            "the new binary's version --json printed no version: {}",
            cut(stdout)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_asset_follows_the_architecture() {
        assert_eq!(asset_for(Arch::Amd64), "sb_linux_amd64");
        assert_eq!(asset_for(Arch::Arm64), "sb_linux_arm64");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn an_x86_64_build_asks_for_the_amd64_asset() {
        assert_eq!(asset_name().expect("this build's"), "sb_linux_amd64");
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn an_aarch64_build_asks_for_the_arm64_asset() {
        assert_eq!(asset_name().expect("this build's"), "sb_linux_arm64");
    }

    #[test]
    fn the_new_binary_must_report_the_tag() {
        let json = r#"{"version":"2.0.0","commit":"abc1234","uv_version":"0.9.0"}"#;
        assert_eq!(reports(json, "v2.0.0"), Ok(()));
        assert_eq!(reports(&format!("{json}\n"), "2.0.0"), Ok(()));
        assert_eq!(
            reports(r#"{"version":"1.9.0"}"#, "v2.0.0"),
            Err("the new binary reports version 1.9.0, not 2.0.0".to_owned())
        );
        // The tag's `v` is dropped; the binary's own isn't.
        assert_eq!(
            reports(r#"{"version":"v2.0.0"}"#, "v2.0.0"),
            Err("the new binary reports version v2.0.0, not 2.0.0".to_owned())
        );
        assert_eq!(
            reports("Saltbox CLI version: 2.0.0\n", "v2.0.0"),
            Err(
                "the new binary's version --json printed no version: Saltbox CLI version: 2.0.0"
                    .to_owned()
            )
        );
        assert_eq!(
            reports(r#"{"commit":"abc1234"}"#, "v2.0.0"),
            Err(
                r#"the new binary's version --json printed no version: {"commit":"abc1234"}"#
                    .to_owned()
            )
        );
        assert_eq!(
            reports(" \n", "v2.0.0"),
            Err("the new binary's version --json printed nothing".to_owned())
        );
    }

    /// What it printed reaches the error made safe: no escape sequence.
    #[test]
    fn what_it_printed_is_made_safe() {
        let err = reports("\u{1b}[31mred\u{1b}[0m", "v2.0.0").expect_err("not JSON");
        assert!(!err.contains('\u{1b}'), "{err:?}");
        let err = reports(r#"{"version":"\u001b[2Jx"}"#, "v2.0.0").expect_err("other");
        assert!(!err.contains('\u{1b}'), "{err:?}");
    }

    #[test]
    fn a_wrong_binary_reads_as_sb_go_s() {
        assert_eq!(
            elf_text(ElfError::WrongMachine(Arch::Amd64)),
            "executable is not a 64-bit x86 ELF binary"
        );
        assert_eq!(
            elf_text(ElfError::WrongMachine(Arch::Arm64)),
            "executable is not a 64-bit arm64 ELF binary"
        );
        assert_eq!(
            elf_text(ElfError::NotElf("Unknown file magic".to_owned())),
            "open ELF executable: Unknown file magic"
        );
    }
}
