//! `sb self-update`'s flow (release spec, "`sb self-update`"), sb-go's
//! `selfupdate.Run` with the spec's decisions: off on the `none` channel
//! and in raw builds unless forced; the newest release of the build's
//! channel looked up, or of another one asked for once (`--channel`,
//! "Release channels"); the user asked on the terminal (or `--yes`);
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
use sb_cli_core::build_info::{BuildInfo, Channel};
use sb_cli_core::cancel::Shutdown;
use sb_exec::Cmd;
use sb_term_ui::{Labels, Runner, Step};
use semver::Version;

use crate::lookup::cut;
use crate::{Arch, ElfError, GithubRepo, Release, Releases, Stage, Track, activate, check_elf};

/// The question, sb-go's with a question mark, as sb's other questions
/// have.
const QUESTION: &str = "Do you want to update?";
/// The question when `--channel` switches to another channel.
const SWITCH_QUESTION: &str = "Do you want to switch?";
/// The warning under the check's step when the question is declined
/// (sb-go's text).
const CANCELLED: &str = "Update of sb CLI cancelled";
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
    /// `--channel`: the channel to follow this once (the build's own when
    /// `None`); another than the build's installs that channel's newest
    /// release whatever its version.
    pub switch_to: Option<Channel>,
    /// Shown as a warning after a successful update (`sb update`'s).
    pub message: Option<&'a str>,
    /// The run's shutdown, which a Ctrl-C at the question interrupts: the
    /// one the `runner` closure's runners are started with
    /// ([`self_update`]), since `Runner` doesn't hand its own out.
    pub shutdown: &'a Shutdown,
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

/// The asset for this build's architecture on `channel` (release spec,
/// decision 9 and "Release channels"): `sb_linux_<arch>`, with `_beta` on
/// the beta channel.
///
/// # Errors
///
/// An architecture with no sb release.
pub fn asset_name(channel: Channel) -> anyhow::Result<&'static str> {
    Ok(asset_for(Arch::current()?, channel))
}

fn asset_for(arch: Arch, channel: Channel) -> &'static str {
    match (arch, channel == Channel::Beta) {
        (Arch::Amd64, false) => "sb_linux_amd64",
        (Arch::Amd64, true) => "sb_linux_amd64_beta",
        (Arch::Arm64, false) => "sb_linux_arm64",
        (Arch::Arm64, true) => "sb_linux_arm64_beta",
    }
}

/// The channel a self-update follows: `--channel`'s, else the build's; a
/// forced update of a `none` build follows `release`.
fn followed(update: &SelfUpdate<'_>) -> Channel {
    match asked(update).unwrap_or(update.build.channel) {
        Channel::None => Channel::Release,
        channel => channel,
    }
}

/// `--channel`'s channel; `none` asks for no channel, so it is no switch.
fn asked(update: &SelfUpdate<'_>) -> Option<Channel> {
    update.switch_to.filter(|channel| *channel != Channel::None)
}

/// The track a channel's lookup follows.
fn track(channel: Channel) -> Track {
    if channel == Channel::Beta {
        Track::Prereleases
    } else {
        Track::Stable
    }
}

/// Run a self-update on `runner` (the caller's, titled and logged by it:
/// one tree for the check, the question and the install, term-ui decision
/// 14): the check's step; the question, unless `update.yes` answers it, on
/// the alternate screen with the runner paused over it, its answer a
/// notice under the check's step (release spec, "Ask"); then the install's
/// step. The caller finishes the runner.
///
/// # Errors
///
/// The step's [`Exit`](sb_cli_core::exit::Exit) when a step failed (its
/// error shown in the tree: `check for updates: ...`, `binary update
/// failed: ...`), or the prompt's error (no terminal and no `--yes`).
pub async fn self_update(
    releases: &Releases,
    update: SelfUpdate<'_>,
    runner: &mut Runner,
) -> anyhow::Result<Outcome> {
    let build = update.build;
    // A raw build counts as disabled (decision 11): a developer build
    // never replaces itself with a release.
    let disabled = build.self_update_off();
    let found = runner
        .step(
            Labels::new("Checking for sb CLI updates", "Checked for sb CLI updates"),
            async |step| {
                if disabled && !update.force {
                    // Notices under the step (the maintainer, 2026-11-18):
                    // the text sb-go's CI greps is kept.
                    step.info("Self-update is disabled in this build");
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
                let channel = followed(&update);
                // A switch is `--channel` naming another channel than the
                // build's; a forced update of a `none` build follows
                // `release` as an update does.
                let switching = asked(&update).is_some_and(|asked| asked != build.channel);
                let asset = asset_for(arch, channel);
                // A switch installs the channel's newest whatever its
                // version: back to `release` from a newer rc is lower.
                let Some(newest) = releases
                    .newest(
                        update.repo,
                        track(channel),
                        (!switching).then_some(&current),
                        asset,
                        step,
                    )
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
                let mut context = Vec::new();
                if switching {
                    context.push(format!(
                        "Switching sb CLI to the {} channel: {version} (current: {current}, {})",
                        channel.name(),
                        build.channel.name()
                    ));
                    step.info(context[0].as_str());
                    if version.cmp_precedence(&current) == std::cmp::Ordering::Less {
                        let older = format!("{version} is older than the running {current}");
                        step.warn(older.as_str());
                        context.push(older);
                    }
                } else {
                    context.push(format!(
                        "New sb CLI version available: {version} (current: {current})"
                    ));
                    step.info(context[0].as_str());
                }
                Ok(Some(Found {
                    context: context.join("\n"),
                    release: newest,
                    version,
                    arch,
                    channel,
                    switching,
                }))
            },
        )
        .await;
    let Some(found) = found? else {
        return Ok(if disabled && !update.force {
            Outcome::Disabled
        } else {
            Outcome::Latest
        });
    };
    if !accept(&update, &found, runner)? {
        return Ok(Outcome::Declined);
    }
    install(releases, update, &found, runner).await?;
    Ok(Outcome::Updated(found.version))
}

/// The question, unless `--yes` answers it: on the alternate screen, the
/// tree paused over it, what the check found shown with it (term-ui
/// decision 14); the answer a notice under the check's step, the
/// declined one sb-go's warning.
///
/// # Errors
///
/// The prompt's (no terminal and no `--yes`, Ctrl-C).
fn accept(update: &SelfUpdate<'_>, found: &Found, runner: &mut Runner) -> anyhow::Result<bool> {
    if !update.yes {
        runner.pause_over();
    }
    let question = if found.switching {
        SWITCH_QUESTION
    } else {
        QUESTION
    };
    let accepted = sb_term_ui::confirm_over(
        question,
        &found.context,
        update.yes.then_some(true),
        "--yes",
        update.shutdown,
    )?;
    if !accepted {
        runner.warn(CANCELLED);
    } else if !update.yes {
        runner.info(format!(
            "Update to {} accepted (your answer)",
            found.version
        ));
    }
    Ok(accepted)
}

/// The release the lookup found, its version, and the architecture and
/// channel its asset was chosen for; whether it switches channel.
struct Found {
    /// The check's lines the question is about, shown with it.
    context: String,
    release: Release,
    version: Version,
    arch: Arch,
    channel: Channel,
    switching: bool,
}

/// Download the release's asset next to the target, check it as a binary
/// of the found architecture and version, and put it in place, on
/// `updating`'s steps.
async fn install(
    releases: &Releases,
    update: SelfUpdate<'_>,
    found: &Found,
    updating: &mut Runner,
) -> anyhow::Result<()> {
    let Found {
        release: newest,
        version,
        arch,
        channel,
        ..
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
                        check_new(staged.path(), *arch, &newest.tag, *channel, step)
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
    Ok(done?)
}

/// The staged binary's checks before it replaces sb (decision 12,
/// decided.md D-Q5): an ELF for `arch`; then, made executable, its own
/// `version --json`, run with an empty environment and a timeout, must
/// report `tag` without its `v`, and `channel` ("Release channels"), so a
/// mislabelled asset can't switch a host's channel.
async fn check_new(
    staged: &Path,
    arch: Arch,
    tag: &str,
    channel: Channel,
    step: &mut Step,
) -> anyhow::Result<()> {
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
    reports(&ran.stdout, tag, channel).map_err(anyhow::Error::msg)
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

/// Whether `stdout`, a `version --json`, reports `tag` without its `v`
/// and `channel`; else why not, naming what it printed (made safe and
/// cut).
fn reports(stdout: &str, tag: &str, channel: Channel) -> Result<(), String> {
    /// The fields read; sb's others are ignored. Why not `Channel` with
    /// serde: an unknown value would be a parse error, and the refusal
    /// would lose the value it names.
    #[derive(serde::Deserialize)]
    struct Reported {
        version: String,
        channel: Option<String>,
    }
    let want = tag.strip_prefix('v').unwrap_or(tag);
    match serde_json::from_str::<Reported>(stdout) {
        Ok(got) if got.version != want => Err(format!(
            "the new binary reports version {}, not {want}",
            cut(&got.version)
        )),
        Ok(got) if got.channel.as_deref() == Some(channel.name()) => Ok(()),
        Ok(got) => Err(format!(
            "the new binary reports channel {}, not {}",
            got.channel
                .as_deref()
                .map_or_else(|| "none given".to_owned(), cut),
            channel.name()
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

    /// The asset follows the architecture, and the beta channel adds
    /// `_beta`; `none` (a forced update) asks for the plain one.
    #[test]
    fn the_asset_follows_the_architecture_and_the_channel() {
        assert_eq!(asset_for(Arch::Amd64, Channel::Release), "sb_linux_amd64");
        assert_eq!(asset_for(Arch::Arm64, Channel::Release), "sb_linux_arm64");
        assert_eq!(asset_for(Arch::Amd64, Channel::Beta), "sb_linux_amd64_beta");
        assert_eq!(asset_for(Arch::Arm64, Channel::Beta), "sb_linux_arm64_beta");
        assert_eq!(asset_for(Arch::Amd64, Channel::None), "sb_linux_amd64");
        assert_eq!(track(Channel::Release), Track::Stable);
        assert_eq!(track(Channel::Beta), Track::Prereleases);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn an_x86_64_build_asks_for_the_amd64_asset() {
        assert_eq!(
            asset_name(Channel::Release).expect("this build's"),
            "sb_linux_amd64"
        );
        assert_eq!(
            asset_name(Channel::Beta).expect("this build's"),
            "sb_linux_amd64_beta"
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn an_aarch64_build_asks_for_the_arm64_asset() {
        assert_eq!(
            asset_name(Channel::Release).expect("this build's"),
            "sb_linux_arm64"
        );
        assert_eq!(
            asset_name(Channel::Beta).expect("this build's"),
            "sb_linux_arm64_beta"
        );
    }

    /// `--channel none` is no switch: the build's channel is followed,
    /// `release` for a forced `none` build.
    #[test]
    fn asking_for_none_is_no_switch() {
        let shutdown = Shutdown::new();
        let update = |build: &BuildInfo, switch_to| {
            let update = SelfUpdate {
                repo: GithubRepo {
                    owner: "o",
                    name: "r",
                },
                build,
                target: Path::new("/x"),
                yes: true,
                force: false,
                switch_to,
                message: None,
                shutdown: &shutdown,
            };
            (asked(&update), followed(&update))
        };
        let beta = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, Channel::Beta);
        assert_eq!(update(&beta, Some(Channel::None)), (None, Channel::Beta));
        assert_eq!(
            update(&beta, Some(Channel::Release)),
            (Some(Channel::Release), Channel::Release)
        );
        assert_eq!(update(&beta, None), (None, Channel::Beta));
        let none = BuildInfo::from_env("0.1.0", Some("1.0.0"), None, Channel::None);
        assert_eq!(update(&none, Some(Channel::None)), (None, Channel::Release));
    }

    /// The new binary must report the tag's version and the channel asked
    /// for: a mislabelled asset can't switch a host's channel.
    #[test]
    fn the_new_binary_must_report_the_channel() {
        let beta = r#"{"version":"2.0.0","channel":"beta"}"#;
        assert_eq!(reports(beta, "2.0.0", Channel::Beta), Ok(()));
        assert_eq!(
            reports(beta, "2.0.0", Channel::Release),
            Err("the new binary reports channel beta, not release".to_owned())
        );
        assert_eq!(
            reports(r#"{"version":"2.0.0"}"#, "2.0.0", Channel::Release),
            Err("the new binary reports channel none given, not release".to_owned())
        );
        let err = reports(
            r#"{"version":"2.0.0","channel":"\u001b[2Jx"}"#,
            "2.0.0",
            Channel::Beta,
        )
        .expect_err("other");
        assert!(!err.contains('\u{1b}'), "{err:?}");
    }

    #[test]
    fn the_new_binary_must_report_the_tag() {
        let json =
            r#"{"version":"2.0.0","commit":"abc1234","uv_version":"0.9.0","channel":"release"}"#;
        let reports = |stdout: &str, tag: &str| reports(stdout, tag, Channel::Release);
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
        let reports = |stdout: &str, tag: &str| reports(stdout, tag, Channel::Release);
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
