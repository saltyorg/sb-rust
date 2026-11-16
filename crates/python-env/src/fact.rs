//! `saltbox.fact` (python-env spec, "saltbox.fact"; sb-go's
//! `saltbox/fact.go`; decisions 2, 6, 8, 20 and 21): the latest release
//! of saltyorg/ansible-facts for this build's architecture, the installed
//! fact's `--version` against it (the digest rule at an equal version),
//! and, when it must go, the verified download, the checks of the staged
//! file (size, release's ELF check, its own `--version`) and the
//! activation, all through sb-release (R2-R8).

use std::cmp::Ordering;
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::time::Duration;

use anyhow::Context as _;
use sb_cli_core::exit::Exit;
use sb_exec::Cmd;
use sb_release::{Arch, ElfError, GithubRepo, Query, Release, Stage};
use sb_term_ui::{Labels, Step};
use semver::Version;
use tokio_util::sync::CancellationToken;

use crate::Env;

/// The fact's releases.
pub const FACTS_REPO: GithubRepo = GithubRepo {
    owner: "saltyorg",
    name: "ansible-facts",
};

/// Each `--version` run's limit (sb-go's 10 s).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The download's cap (sb-go's 32 MiB).
const MAX_FACT: u64 = 32 << 20;

/// The staged download's name prefix, in the fact's own directory.
const STAGE_PREFIX: &str = ".saltbox.fact-";

/// Whether the fact is checked against the release (`update-venv`,
/// `update`) or installed whatever it holds (`reinstall-facts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactMode {
    /// Kept when it is the release's (or newer), else installed.
    Update,
    /// Installed whatever it holds (`reinstall-facts`).
    Reinstall,
}

/// What [`install_fact`] did, with the versions its info line names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactOutcome {
    /// Kept: at the release's version with its bytes, or newer.
    UpToDate { version: String },
    /// There was none.
    Installed { version: String },
    /// An older one, or one whose version couldn't be read, replaced.
    Updated { from: Option<String>, to: String },
    /// Forced (`reinstall-facts`).
    Reinstalled { version: String },
    /// The release's version with other bytes, replaced (decision 2).
    Repaired { version: String },
}

/// Why the fact couldn't be checked or installed, in sb-go's words. A
/// nested step (the metadata, the install) shows its own failure and
/// ends this as [`FactError::Stopped`].
#[derive(Debug, thiserror::Error)]
pub enum FactError {
    /// The installed file couldn't be looked at or read.
    #[error("error checking for existing saltbox.fact: {0}")]
    Check(io::Error),
    /// A nested step failed or was interrupted; it says why itself.
    #[error("a step stopped the saltbox.fact update")]
    Stopped(#[source] Exit),
}

/// Why a staged fact isn't one to activate (sb-go's `validateBinary`
/// texts, decision 20's version check).
#[derive(Debug, thiserror::Error)]
pub(crate) enum ValidateError {
    #[error("cannot stat file: {0}")]
    Stat(io::Error),
    #[error("file size mismatch: expected {expected} bytes, got {got} bytes")]
    Size { expected: u64, got: u64 },
    #[error("file is not a valid ELF binary: {0}")]
    NotElf(String),
    #[error("file is not an {} ELF binary", machine(*.0))]
    WrongMachine(Arch),
    #[error("set the staged saltbox.fact's permissions: {0}")]
    Permissions(io::Error),
    #[error("{0}")]
    Run(String),
    #[error("downloaded saltbox.fact reports version {got}, expected {want}")]
    Version { got: String, want: String },
}

/// sb-go's name for `arch`'s machine in its ELF text (fact.go:78-82).
fn machine(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "x86-64",
        Arch::Arm64 => "aarch64",
    }
}

impl From<ElfError> for ValidateError {
    fn from(err: ElfError) -> Self {
        match err {
            ElfError::NotElf(cause) => Self::NotElf(cause),
            ElfError::WrongMachine(arch) => Self::WrongMachine(arch),
        }
    }
}

/// The fact's asset for `arch` (decisions 8 and 21).
#[must_use]
pub fn asset_name(arch: Arch) -> String {
    format!("saltbox-facts-{}", arch.go_name())
}

/// The version in a fact's `--version` output (sb-go's
/// `parseFactVersionOutput`, e82c34d): one plain line of strict semver,
/// `v` optional, or legacy JSON's `saltbox_facts_version`.
///
/// # Errors
///
/// sb-go's text for what it isn't.
pub(crate) fn parse_version_output(output: &str) -> Result<String, String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Err("fact version output is empty".to_owned());
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let data: serde_json::Map<String, serde_json::Value> = serde_json::from_str(trimmed)
            .map_err(|err| format!("parsing legacy fact JSON: {err}"))?;
        let raw = data
            .get("saltbox_facts_version")
            .ok_or("legacy fact JSON has no saltbox_facts_version")?;
        let version = raw.as_str().ok_or_else(|| {
            format!(
                "parsing saltbox_facts_version from legacy fact JSON: \
                 expected a string, got {raw}"
            )
        })?;
        let version = version.trim();
        if version.is_empty() {
            return Err("legacy fact JSON has an empty saltbox_facts_version".to_owned());
        }
        return Ok(version.to_owned());
    }
    if trimmed.contains(['\r', '\n']) {
        return Err("plain fact version output must be one line".to_owned());
    }
    Version::parse(trimmed.strip_prefix('v').unwrap_or(trimmed))
        .map_err(|err| format!("invalid plain fact version {trimmed:?}: {err}"))?;
    Ok(trimmed.to_owned())
}

/// `text` as a version, `v` optional (sb-go trims it before
/// `semver.NewVersion`).
fn version(text: &str) -> Result<Version, semver::Error> {
    let text = text.trim();
    Version::parse(text.strip_prefix('v').unwrap_or(text))
}

/// What the versions say (sb-go's `checkIfUpdateNeeded`, by precedence
/// as Masterminds' `Compare`): older is updated, newer kept, and an equal
/// one is kept only with the release's bytes (decision 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Update,
    Keep,
    CheckDigest,
}

fn action(installed: &Version, latest: &Version) -> Action {
    match installed.cmp_precedence(latest) {
        Ordering::Less => Action::Update,
        Ordering::Greater => Action::Keep,
        Ordering::Equal => Action::CheckDigest,
    }
}

/// `fact --version`, captured within 10 s, run from `/` (never bare: that
/// run makes network calls and writes the IP cache).
async fn run_version(path: &std::path::Path, token: &CancellationToken) -> Result<String, String> {
    let cmd = Cmd::new(path.as_os_str())
        .arg("--version")
        .cwd("/")
        .timeout(PROBE_TIMEOUT);
    crate::captured(&cmd, token).await
}

/// A nested step's body under `labels`, its failure (shown by the step)
/// ending this as [`FactError::Stopped`].
async fn nested<T>(
    step: &mut Step,
    labels: Labels,
    body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<T>,
) -> Result<T, FactError> {
    step.step(labels, body).await.map_err(FactError::Stopped)
}

/// `saltbox.fact` brought in line with its latest release (spec,
/// "saltbox.fact"; sb-go's `downloadAndInstallSaltboxFact`), or
/// reinstalled whatever it holds: "Fetching latest saltbox.fact release
/// info", the decision, and when it must go, "Updating saltbox.fact to
/// version <v>" (or "Reinstalling ...") holding "Validating downloaded
/// saltbox.fact". sb-go's info and warning lines go on `step`.
///
/// # Errors
///
/// [`FactError`]: the installed fact can't be looked at, or a nested
/// step (the metadata, the install) failed and says why.
pub async fn install_fact(
    step: &mut Step,
    env: &Env<'_>,
    mode: FactMode,
) -> Result<FactOutcome, FactError> {
    let asset = asset_name(env.arch);
    let fetching = "Fetching latest saltbox.fact release info";
    let release = nested(step, Labels::new(fetching, fetching), async |step| {
        Ok(env
            .release
            .release(FACTS_REPO, Query::Latest, &asset, step)
            .await?)
    })
    .await?;
    let outcome = decide(step, env, &release, mode).await?;
    let tag = &release.tag;
    let label = match &outcome {
        FactOutcome::UpToDate { version } => {
            step.info(format!("saltbox.fact is up to date (version {version})"));
            return Ok(outcome);
        }
        FactOutcome::Reinstalled { .. } | FactOutcome::Repaired { .. } => {
            format!("Reinstalling saltbox.fact with version {tag}")
        }
        FactOutcome::Installed { .. } | FactOutcome::Updated { .. } => {
            format!("Updating saltbox.fact to version {tag}")
        }
    };
    nested(step, Labels::new(&label, &label), async |step| {
        install(step, env, &release).await
    })
    .await?;
    step.info(match &outcome {
        FactOutcome::Updated {
            from: Some(from),
            to,
        } => format!("saltbox.fact updated successfully: {from} → {to}"),
        FactOutcome::Updated { from: None, to } | FactOutcome::Installed { version: to } => {
            format!("saltbox.fact installed successfully (version {to})")
        }
        // Reinstalled and Repaired; UpToDate returned above.
        _ => format!("saltbox.fact reinstalled successfully (version {tag})"),
    });
    Ok(outcome)
}

/// What to do (sb-go's `checkIfUpdateNeeded` plus the digest rule), its
/// warning or info line shown here; [`FactOutcome::UpToDate`] keeps the
/// fact.
async fn decide(
    step: &mut Step,
    env: &Env<'_>,
    release: &Release,
    mode: FactMode,
) -> Result<FactOutcome, FactError> {
    let latest = release.tag.clone();
    if mode == FactMode::Reinstall {
        step.info("Reinstall forced.");
        return Ok(FactOutcome::Reinstalled { version: latest });
    }
    let fact = &env.layout.fact;
    match std::fs::metadata(fact) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            step.info("saltbox.fact not found. Proceeding with update.");
            return Ok(FactOutcome::Installed { version: latest });
        }
        Err(err) => return Err(FactError::Check(err)),
        Ok(_) => {}
    }
    let current = run_version(fact, &step.token())
        .await
        .map_err(|cause| format!("failed to run saltbox.fact: {cause}"))
        .and_then(|output| parse_version_output(&output));
    let current = match current {
        Ok(current) => current,
        Err(cause) => {
            step.warn(format!("{cause}. Proceeding with update."));
            return Ok(FactOutcome::Updated {
                from: None,
                to: latest,
            });
        }
    };
    let installed = match version(&current) {
        Ok(installed) => installed,
        Err(err) => {
            step.warn(format!(
                "Failed to parse current version: {err}. Updating..."
            ));
            return Ok(FactOutcome::Updated {
                from: Some(current),
                to: latest,
            });
        }
    };
    let newest = match version(&latest) {
        Ok(newest) => newest,
        Err(err) => {
            step.warn(format!(
                "Failed to parse latest version: {err}. Updating..."
            ));
            return Ok(FactOutcome::Updated {
                from: Some(current),
                to: latest,
            });
        }
    };
    Ok(match action(&installed, &newest) {
        Action::Update => FactOutcome::Updated {
            from: Some(current),
            to: latest,
        },
        Action::Keep => FactOutcome::UpToDate { version: current },
        Action::CheckDigest => {
            // Up to 32 MiB read off the runtime's one thread, which also
            // draws the steps
            // (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>);
            // a panic resumed, as sb-host's `unpanicked` does; the run's
            // span goes along (<https://docs.rs/tracing/0.1.44/tracing/struct.Span.html#method.in_scope>).
            let path = fact.clone();
            let span = tracing::Span::current();
            let got = tokio::task::spawn_blocking(move || {
                span.in_scope(|| sb_release::sha256_file(&path))
            })
            .await
            .unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()))
            .map_err(FactError::Check)?;
            let want = release.asset.sha256();
            if got == want {
                FactOutcome::UpToDate { version: current }
            } else {
                step.warn(format!(
                    "saltbox.fact {current} does not match its release \
                     (sha256 {}, expected {}). Reinstalling.",
                    hex::encode(got),
                    hex::encode(want)
                ));
                FactOutcome::Repaired { version: latest }
            }
        }
    })
}

/// The install step's body (sb-go's, fact.go:270-285): the verified
/// download into the fact's directory (Saltbox-user-owned, so the staged
/// file is checked, run and activated by its path as sb-go does; its
/// writer is the host's admin, whose fact Ansible runs as root anyway:
/// the spec's known limits), "Validating downloaded
/// saltbox.fact", then the activation, 0755, the directory synced
/// (release's `activate`). The staged file is removed on any failure
/// (release's `Staged`, dropped).
async fn install(step: &mut Step, env: &Env<'_>, release: &Release) -> anyhow::Result<()> {
    let fact = &env.layout.fact;
    let dir = fact.parent().unwrap_or(std::path::Path::new("/"));
    let stage = Stage {
        dir,
        prefix: STAGE_PREFIX,
        max_size: MAX_FACT,
    };
    // Saltbox's checkout tracks no `ansible_facts.d` (only the fact, which
    // it ignores), so a fresh one has none: made 0755 as sb-go's
    // `DownloadVerified` makes its staging directory (release/asset.go,
    // `os.MkdirAll(dir, 0755)`), with its text, through std's
    // `DirBuilderExt::mode`
    // (https://doc.rust-lang.org/std/os/unix/fs/trait.DirBuilderExt.html).
    // Made before the request, where sb-go makes it after the response's
    // 200: a failed download may leave the empty directory, which the
    // next run fills. Why not in sb-release's `stage`: its caller owns the
    // directory (release spec, "in the caller's directory"), and its other
    // callers stage into directories that exist.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
        .context("create release asset staging directory")
        .context("error downloading saltbox.fact")?;
    let staged = env
        .release
        .download(&release.asset, stage, step)
        .await
        .context("error downloading saltbox.fact")?;
    let validating = "Validating downloaded saltbox.fact";
    step.step(Labels::new(validating, validating), async |step| {
        validate(staged.path(), env.arch, release, &step.token())
            .await
            .context("downloaded binary validation failed")
    })
    .await?;
    sb_release::activate(staged, fact, 0o755)?;
    Ok(())
}

/// The staged fact's checks (sb-go's `validateBinary`, decision 20): its
/// size the API's, an ELF for `arch` (release's `check_elf`), and, made
/// executable, its `--version` the release's tag.
async fn validate(
    staged: &std::path::Path,
    arch: Arch,
    release: &Release,
    token: &CancellationToken,
) -> Result<(), ValidateError> {
    let got = std::fs::metadata(staged)
        .map_err(ValidateError::Stat)?
        .len();
    let expected = release.asset.size();
    if got != expected {
        return Err(ValidateError::Size { expected, got });
    }
    sb_release::check_elf(staged, arch)?;
    // Made executable to run it, as self-update's staged binary is.
    std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755))
        .map_err(ValidateError::Permissions)?;
    let output = run_version(staged, token)
        .await
        .map_err(ValidateError::Run)?;
    let got = parse_version_output(&output).map_err(ValidateError::Run)?;
    // The tag itself, each without its `v` (`v1.4.0` is `1.4.0`): the
    // output is strict semver already, so text is the whole comparison.
    let bare = |text: &str| text.strip_prefix('v').unwrap_or(text).to_owned();
    if bare(&got) != bare(&release.tag) {
        return Err(ValidateError::Version {
            got,
            want: release.tag.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_asset_is_named_as_go_names_the_architecture() {
        assert_eq!(asset_name(Arch::Amd64), "saltbox-facts-amd64");
        assert_eq!(asset_name(Arch::Arm64), "saltbox-facts-arm64");
    }

    /// sb-go's `parseFactVersionOutput` cases (e82c34d).
    #[test]
    fn a_plain_or_legacy_version_is_read() {
        for (output, want) in [
            ("1.4.0\n", "1.4.0"),
            ("  v1.4.0  ", "v1.4.0"),
            (
                r#"{"saltbox_facts_version": " 1.2.3 ", "other": 1}"#,
                "1.2.3",
            ),
        ] {
            assert_eq!(
                parse_version_output(output).as_deref(),
                Ok(want),
                "{output}"
            );
        }
    }

    #[test]
    fn other_output_is_named_as_sb_go_names_it() {
        for (output, want) in [
            ("", "fact version output is empty"),
            (" \n", "fact version output is empty"),
            ("1.4.0\n1.4.1", "plain fact version output must be one line"),
            (
                r#"{"other": 1}"#,
                "legacy fact JSON has no saltbox_facts_version",
            ),
            (
                r#"{"saltbox_facts_version": " "}"#,
                "legacy fact JSON has an empty saltbox_facts_version",
            ),
        ] {
            assert_eq!(
                parse_version_output(output),
                Err(want.to_owned()),
                "{output:?}"
            );
        }
        for (output, starts) in [
            ("{not json", "parsing legacy fact JSON: "),
            ("[1, 2]", "parsing legacy fact JSON: "),
            (
                r#"{"saltbox_facts_version": 3}"#,
                "parsing saltbox_facts_version from legacy fact JSON: ",
            ),
            ("1.4", "invalid plain fact version \"1.4\": "),
            ("saltbox.fact 1.4.0", "invalid plain fact version "),
        ] {
            let err = parse_version_output(output).expect_err(output);
            assert!(err.starts_with(starts), "{output}: {err}");
        }
    }

    /// Precedence, as sb-go's `Compare`: build metadata is no newer.
    #[test]
    fn versions_decide_by_precedence() {
        let v = |text: &str| version(text).expect(text);
        assert_eq!(action(&v("1.3.9"), &v("v1.4.0")), Action::Update);
        assert_eq!(action(&v("1.5.0"), &v("1.4.0")), Action::Keep);
        assert_eq!(action(&v("v1.4.0"), &v("1.4.0")), Action::CheckDigest);
        assert_eq!(action(&v("1.4.0+b1"), &v("1.4.0")), Action::CheckDigest);
        assert!(version("latest").is_err());
    }

    #[test]
    fn the_elf_errors_are_sb_gos() {
        assert_eq!(
            ValidateError::from(ElfError::WrongMachine(Arch::Amd64)).to_string(),
            "file is not an x86-64 ELF binary"
        );
        assert_eq!(
            ValidateError::from(ElfError::WrongMachine(Arch::Arm64)).to_string(),
            "file is not an aarch64 ELF binary"
        );
        assert_eq!(
            ValidateError::from(ElfError::NotElf("Unknown file magic".to_owned())).to_string(),
            "file is not a valid ELF binary: Unknown file magic"
        );
    }
}
