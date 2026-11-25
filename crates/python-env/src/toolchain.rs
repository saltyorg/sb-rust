//! The toolchain Saltbox pins (python-env spec, "The toolchain"; sb-go's
//! `python/toolchain.go` and `venv.go:76-104`, 69b968b): `.python-version`,
//! `.uv-version` as a minimum against the uv this sb provides, the
//! lock's sha256 and the checkout's commit ([`read_pins`]), then the
//! Python resolved against the pinned uv's download list ([`resolve`]).
//! A checkout without `.python-version` falls back to the newest
//! [`FALLBACK_MINOR`] patch, installed unhashed (ADR-031, D7).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_exec::Cmd;
use tokio_util::sync::CancellationToken;

use crate::Env;
use crate::isolate::Kind;

/// `git rev-parse HEAD`'s limit (spec, difference 8).
const COMMIT_TIMEOUT: Duration = Duration::from_secs(10);

/// `uv python list`'s limit: it reads the list built into uv, no network.
const LIST_TIMEOUT: Duration = Duration::from_secs(30);

/// The Python minor of a checkout without `.python-version`, such as
/// Saltbox's pre-* branches: the one sb-go used before Saltbox pinned
/// its Python (sb-go's `FallbackPythonMinor`).
pub const FALLBACK_MINOR: &str = "3.12";

/// What the checkout pins, read before anything changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pins {
    /// `.python-version`: `major.minor.patch`; `None` without the file.
    pub python: Option<String>,
    /// `.uv-version`: the oldest uv Saltbox accepts; `None` without the
    /// file, no minimum.
    pub minimum_uv: Option<String>,
    /// The lock's sha256, lower-case hex.
    pub lock_sha256: String,
    /// The checkout's `HEAD`.
    pub commit: String,
}

/// The toolchain the venv is built for: [`Pins`] with the Python
/// resolved ([`resolve`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// The exact Python: the pin, or the fallback's patch.
    pub python: String,
    /// Whether the requirements are a hashed lock: true when the checkout
    /// pins its Python, false for the fallback's unhashed list.
    pub hashed: bool,
    /// The lock's sha256, lower-case hex.
    pub lock_sha256: String,
    /// The checkout's `HEAD`.
    pub commit: String,
}

impl Toolchain {
    /// The line a run shows when the checkout pins no Python (sb-go's
    /// `Toolchain.Notice`), naming the Python and `uv`, the uv this sb
    /// provides; `None` for a pinned checkout.
    #[must_use]
    pub fn notice(&self, uv: &str) -> Option<String> {
        (!self.hashed).then(|| {
            format!(
                "This Saltbox branch has no pinned Python; using Python {}, the newest {FALLBACK_MINOR} this sb's uv ({uv}) provides",
                self.python
            )
        })
    }
}

/// Why the toolchain couldn't be read, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum ToolchainError {
    /// A pin file couldn't be read.
    #[error("load Saltbox Python toolchain: read {name} version from {}: {cause}", path.display())]
    Read {
        name: &'static str,
        path: PathBuf,
        cause: io::Error,
    },
    /// A pin file holds something else than one exact version.
    #[error(
        "load Saltbox Python toolchain: {} must contain one exact major.minor.patch version, got {value:?}",
        path.display()
    )]
    Malformed { path: PathBuf, value: String },
    /// A version semver can't read (a part with a leading zero).
    #[error("invalid {which} version: {cause}")]
    Version {
        which: &'static str,
        cause: semver::Error,
    },
    /// This sb's uv is older than the checkout's minimum.
    #[error(
        "saltbox requires uv {minimum} or newer, but this sb release provides {provided}; run sb self-update"
    )]
    UvTooOld { minimum: String, provided: String },
    /// The lock couldn't be read.
    #[error("hash Saltbox requirements lock: {0}")]
    Lock(io::Error),
    /// git couldn't tell the checkout's commit.
    #[error("read Saltbox commit: {0}")]
    Commit(String),
    /// uv's download list couldn't be had: the call's failure, uv's own
    /// message in it.
    #[error("list the Pythons uv {uv} can install: {cause}")]
    List { uv: String, cause: String },
    /// uv's download list isn't the JSON uv writes.
    #[error("parse the Pythons uv {uv} can install: {cause}")]
    ListParse {
        uv: String,
        cause: serde_json::Error,
    },
    /// The pinned Python isn't in uv's list (ADR-031, D8).
    #[error(
        "this Saltbox branch needs Python {python}, which this sb's uv ({uv}) can't install; run sb self-update"
    )]
    Unavailable { python: String, uv: String },
    /// A checkout without a pin, and no Python of the fallback's minor in
    /// uv's list.
    #[error(
        "this Saltbox branch has no pinned Python, and this sb's uv ({uv}) offers no Python {minor}; run sb self-update"
    )]
    NoFallback { uv: String, minor: &'static str },
}

/// One `major.minor.patch` of digits (sb-go's `^[0-9]+\.[0-9]+\.[0-9]+$`).
fn exact(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// The exact version in the pin file at `path` (sb-go's
/// `readOptionalExactVersion`), trimmed; `None` when there is no file.
///
/// # Errors
///
/// [`ToolchainError::Read`] or [`ToolchainError::Malformed`].
pub fn read_pin(path: &Path, name: &'static str) -> Result<Option<String>, ToolchainError> {
    let text = match std::fs::read_to_string(path) {
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => return Ok(None),
        read => read.map_err(|cause| ToolchainError::Read {
            name,
            path: path.to_path_buf(),
            cause,
        })?,
    };
    let value = text.trim();
    if exact(value) {
        Ok(Some(value.to_string()))
    } else {
        Err(ToolchainError::Malformed {
            path: path.to_path_buf(),
            value: value.to_string(),
        })
    }
}

/// Refuse a uv older than `minimum` (decision 14: compared with semver,
/// <https://docs.rs/semver/1.0.28/semver/struct.Version.html>).
///
/// # Errors
///
/// [`ToolchainError::UvTooOld`], or [`ToolchainError::Version`] when
/// either isn't a version semver reads.
pub fn check_uv_minimum(minimum: &str, provided: &str) -> Result<(), ToolchainError> {
    let parse = |which, value| {
        semver::Version::parse(value).map_err(|cause| ToolchainError::Version { which, cause })
    };
    let wanted = parse("minimum", minimum)?;
    let have = parse("actual", provided)?;
    if have < wanted {
        return Err(ToolchainError::UvTooOld {
            minimum: minimum.to_string(),
            provided: provided.to_string(),
        });
    }
    Ok(())
}

/// The sha256 of the file at `path`, lower-case hex: release's streaming
/// `sha256_file` (release spec amendment R8) in hex.
///
/// # Errors
///
/// The file's read error.
pub fn file_sha256(path: &Path) -> io::Result<String> {
    sb_release::sha256_file(path).map(hex::encode)
}

/// The checkout's pins (sb-go's `Reconcile`, its first lines): both pin
/// files, the uv minimum against `env.uv_version` when there is one, the
/// lock's digest and `git rev-parse HEAD` in the checkout (capture, 10 s).
///
/// # Errors
///
/// [`ToolchainError`]: the first pin, minimum, lock or commit failure.
pub async fn read_pins(env: &Env<'_>, token: &CancellationToken) -> Result<Pins, ToolchainError> {
    let layout = env.layout;
    let python = read_pin(&layout.python_pin, "Python")?;
    let minimum_uv = read_pin(&layout.uv_pin, "uv")?;
    if let Some(minimum) = &minimum_uv {
        check_uv_minimum(minimum, env.uv_version)?;
    }
    let lock_sha256 = file_sha256(&layout.lock).map_err(ToolchainError::Lock)?;
    let commit = commit(env, token).await?;
    Ok(Pins {
        python,
        minimum_uv,
        lock_sha256,
        commit,
    })
}

/// The Python for `pins` against the Pythons the installed uv can
/// install ([`downloads`]; sb-go's `resolveToolchainWithRuntime`), before
/// anything is installed: a pinned Python when uv lists it, with the
/// hashed lock; without a pin, the newest [`FALLBACK_MINOR`] patch,
/// unhashed. Runs after uv is installed.
///
/// # Errors
///
/// [`ToolchainError::List`], [`ToolchainError::ListParse`],
/// [`ToolchainError::Unavailable`] or [`ToolchainError::NoFallback`].
pub async fn resolve(
    env: &Env<'_>,
    pins: Pins,
    token: &CancellationToken,
) -> Result<Toolchain, ToolchainError> {
    let Pins {
        python,
        lock_sha256,
        commit,
        ..
    } = pins;
    let available = list_downloads(env, token).await?;
    let (python, hashed) = if let Some(python) = python {
        check_listed(&available, &python, env.uv_version)?;
        (python, true)
    } else {
        (newest_fallback(&available, env.uv_version)?, false)
    };
    Ok(Toolchain {
        python,
        hashed,
        lock_sha256,
        commit,
    })
}

/// One entry of `uv python list --output-format json`: the fields read.
#[derive(serde::Deserialize)]
struct Download {
    version: String,
    implementation: String,
    variant: String,
}

/// The exact `CPython` releases in uv's download list `json` (sb-go's
/// `listPythonDownloadsWithRuntime`): `CPython`'s `default` variant only
/// (no free-threaded or debug build), at an exact `X.Y.Z` (no
/// pre-release), in the list's order.
///
/// # Errors
///
/// serde's, when `json` isn't a list of such entries.
pub fn downloads(json: &str) -> Result<Vec<String>, serde_json::Error> {
    let entries: Vec<Download> = serde_json::from_str(json)?;
    Ok(entries
        .into_iter()
        .filter(|entry| {
            entry.implementation == "cpython" && entry.variant == "default" && exact(&entry.version)
        })
        .map(|entry| entry.version)
        .collect())
}

/// Refuse a `pin` that isn't in `available` (ADR-031, D8): sb's own
/// words, naming `uv`, this sb's uv, and `sb self-update`, rather than
/// uv's error at the install.
///
/// # Errors
///
/// [`ToolchainError::Unavailable`].
pub fn check_listed(available: &[String], pin: &str, uv: &str) -> Result<(), ToolchainError> {
    if available.iter().any(|version| version == pin) {
        Ok(())
    } else {
        Err(ToolchainError::Unavailable {
            python: pin.to_string(),
            uv: uv.to_string(),
        })
    }
}

/// The newest [`FALLBACK_MINOR`] patch in `available`, by number (semver
/// reads each exact version; one it refuses, a leading zero, is skipped).
///
/// # Errors
///
/// [`ToolchainError::NoFallback`] when there is none; `uv` names this
/// sb's uv.
pub fn newest_fallback(available: &[String], uv: &str) -> Result<String, ToolchainError> {
    available
        .iter()
        .filter_map(|version| semver::Version::parse(version).ok())
        .filter(|version| format!("{}.{}", version.major, version.minor) == FALLBACK_MINOR)
        .max()
        .map(|version| version.to_string())
        .ok_or_else(|| ToolchainError::NoFallback {
            uv: uv.to_string(),
            minor: FALLBACK_MINOR,
        })
}

/// `uv python list --only-downloads --all-versions --output-format json`
/// (<https://docs.astral.sh/uv/reference/cli/#uv-python-list>), isolated
/// as every uv call, captured (30 s): the exact `CPython` releases the
/// installed uv can install ([`downloads`]). stdout alone is read, so a
/// warning on stderr doesn't spoil the JSON; a failure carries stderr's
/// last lines, uv's own message.
async fn list_downloads(
    env: &Env<'_>,
    token: &CancellationToken,
) -> Result<Vec<String>, ToolchainError> {
    let uv = env.uv_version;
    let failed = |cause: String| ToolchainError::List {
        uv: uv.to_string(),
        cause,
    };
    let list = env
        .isolated(Kind::Uv { cache: true }, &env.layout.uv)
        .map_err(|err| failed(err.to_string()))?
        .map(|cmd| {
            cmd.arg("python")
                .arg("list")
                .arg("--only-downloads")
                .arg("--all-versions")
                .arg("--output-format")
                .arg("json")
                .timeout(LIST_TIMEOUT)
        });
    let ran = list
        .cmd
        .capture(token)
        .await
        .map_err(|err| failed(format!("{err}: {}", err.cause())))?;
    let ran = ran
        .check()
        .map_err(|failed_run| failed(stderr_text(&failed_run)))?;
    downloads(&ran.stdout).map_err(|cause| ToolchainError::ListParse {
        uv: uv.to_string(),
        cause,
    })
}

/// A failed child's text with every one of stderr's last lines (sb-go
/// adds uv's whole stderr), on one line: exec's message, then each line
/// trimmed and escaped by cli-core's rule, joined by `; `. Why not
/// sb-host's `child_failure`: it keeps stderr's last line only, and uv's
/// error is often an `error:` line and its causes beneath it.
fn stderr_text(failed: &sb_exec::Failed) -> String {
    let lines: Vec<String> = failed
        .last_lines()
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(sb_cli_core::exit::escaped)
        .collect();
    if lines.is_empty() {
        failed.to_string()
    } else {
        format!("{failed}: {}", lines.join("; "))
    }
}

/// `git rev-parse HEAD` in the checkout, with `LC_ALL=C` as sb-repo runs
/// git, so its errors read the same in every locale. Why not sb-repo's
/// `Repo::head`: python-env doesn't depend on repo (capability map,
/// python-env row).
async fn commit(env: &Env<'_>, token: &CancellationToken) -> Result<String, ToolchainError> {
    let cmd = Cmd::new(env.git.as_os_str())
        .arg("rev-parse")
        .arg("HEAD")
        .cwd(&env.layout.checkout)
        .env("LC_ALL", "C")
        .timeout(COMMIT_TIMEOUT);
    let output = crate::captured(&cmd, token)
        .await
        .map_err(ToolchainError::Commit)?;
    Ok(output.trim().to_string())
}

#[cfg(test)]
mod tests {
    use sha2::{Digest as _, Sha256};

    use super::*;

    #[test]
    fn a_pin_is_one_exact_version_trimmed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".python-version");
        std::fs::write(&path, " 3.12.7\n").expect("write");
        assert_eq!(
            read_pin(&path, "Python").expect("pin").as_deref(),
            Some("3.12.7")
        );
    }

    /// sb-go's text for each malformed pin.
    #[test]
    fn a_malformed_pin_is_refused_with_sb_gos_text() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".uv-version");
        for bad in [
            "3.12",
            "3.12.7.rc1",
            "v3.12.7",
            "3.12.x",
            "",
            "3..7",
            "3.12.7\n3.13.0",
        ] {
            std::fs::write(&path, bad).expect("write");
            let err = read_pin(&path, "uv").expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!(
                    "load Saltbox Python toolchain: {} must contain one exact major.minor.patch version, got {:?}",
                    path.display(),
                    bad.trim()
                )
            );
        }
    }

    /// A checkout without the file pins nothing (ADR-031, D7): sb-go
    /// 69b968b's `readOptionalExactVersion`.
    #[test]
    fn a_missing_pin_is_no_pin() {
        let path = Path::new("/nonexistent/.python-version");
        assert_eq!(read_pin(path, "Python").expect("no pin"), None);
    }

    /// A pin that is there but can't be read still fails, naming its file.
    #[test]
    fn an_unreadable_pin_names_its_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".python-version");
        std::fs::create_dir(&path).expect("a directory in its place");
        let err = read_pin(&path, "Python").expect_err("unreadable");
        assert!(
            err.to_string().starts_with(&format!(
                "load Saltbox Python toolchain: read Python version from {}: ",
                path.display()
            )),
            "{err}"
        );
    }

    /// Invented `uv python list --output-format json` output (sb-go's
    /// `pythonListFixture`): the newest 3.12 among `CPython`'s default
    /// builds is 3.12.41 by number, not by text; a pre-release, a
    /// free-threaded build and `PyPy` are left out.
    const DOWNLOADS: &str = r#"[
      {"key": "cpython-3.13.50-linux-x86_64-gnu", "version": "3.13.50", "implementation": "cpython", "variant": "default"},
      {"key": "cpython-3.12.80rc1-linux-x86_64-gnu", "version": "3.12.80rc1", "implementation": "cpython", "variant": "default"},
      {"key": "cpython-3.12.60+freethreaded-linux-x86_64-gnu", "version": "3.12.60", "implementation": "cpython", "variant": "freethreaded"},
      {"key": "pypy-3.12.70-linux-x86_64-gnu", "version": "3.12.70", "implementation": "pypy", "variant": "default"},
      {"key": "cpython-3.12.9-linux-x86_64-gnu", "version": "3.12.9", "implementation": "cpython", "variant": "default"},
      {"key": "cpython-3.12.41-linux-x86_64-gnu", "version": "3.12.41", "implementation": "cpython", "variant": "default"},
      {"key": "cpython-3.12.7-linux-x86_64-gnu", "version": "3.12.7", "implementation": "cpython", "variant": "default"}
    ]"#;

    #[test]
    fn the_download_list_keeps_cpythons_default_exact_releases() {
        assert_eq!(
            downloads(DOWNLOADS).expect("parsed"),
            ["3.13.50", "3.12.9", "3.12.41", "3.12.7"]
        );
        assert!(downloads("not json").is_err());
        assert!(downloads(r#"[{"version": "3.12.7"}]"#).is_err());
    }

    #[test]
    fn the_fallback_is_the_newest_312_by_number() {
        let available = downloads(DOWNLOADS).expect("parsed");
        assert_eq!(
            newest_fallback(&available, "0.90.1").expect("found"),
            "3.12.41"
        );
        // A leading zero semver refuses is skipped, not picked.
        let available = ["3.12.09".to_string(), "3.12.8".to_string()];
        assert_eq!(
            newest_fallback(&available, "0.90.1").expect("found"),
            "3.12.8"
        );
    }

    /// A pin uv's list has is installed; one it lacks, and the same
    /// version as another variant or implementation only, is D8's message
    /// (sb-go's `TestResolveToolchainRefusesPinTheUVCannotInstall`).
    #[test]
    fn a_pin_must_be_in_the_list() {
        let available = downloads(DOWNLOADS).expect("parsed");
        assert!(check_listed(&available, "3.12.9", "0.90.1").is_ok());
        for pin in ["3.12.42", "3.12.60", "3.12.70"] {
            let err = check_listed(&available, pin, "0.90.1").expect_err(pin);
            assert_eq!(
                err.to_string(),
                format!(
                    "this Saltbox branch needs Python {pin}, which this sb's uv (0.90.1) can't install; run sb self-update"
                )
            );
        }
    }

    #[test]
    fn no_312_in_the_list_is_named() {
        let available = [
            "3.13.50".to_string(),
            "3.11.9".to_string(),
            "4.12.1".to_string(),
        ];
        let err = newest_fallback(&available, "0.90.1").expect_err("none");
        assert_eq!(
            err.to_string(),
            "this Saltbox branch has no pinned Python, and this sb's uv (0.90.1) offers no Python 3.12; run sb self-update"
        );
    }

    #[test]
    fn only_an_unhashed_toolchain_has_a_notice() {
        let fallback = Toolchain {
            python: "3.12.41".to_string(),
            hashed: false,
            lock_sha256: "d".repeat(64),
            commit: "c".repeat(40),
        };
        assert_eq!(
            fallback.notice("0.90.1").as_deref(),
            Some(
                "This Saltbox branch has no pinned Python; using Python 3.12.41, the newest 3.12 this sb's uv (0.90.1) provides"
            )
        );
        let pinned = Toolchain {
            hashed: true,
            ..fallback
        };
        assert_eq!(pinned.notice("0.90.1"), None);
    }

    /// By semver, not by text: 0.13.10 is newer than 0.13.4.
    #[test]
    fn the_uv_minimum_compares_by_semver() {
        assert!(check_uv_minimum("0.13.4", "0.13.10").is_ok());
        assert!(check_uv_minimum("0.13.4", "0.13.4").is_ok());
        let err = check_uv_minimum("0.13.10", "0.13.4").expect_err("older");
        assert_eq!(
            err.to_string(),
            "saltbox requires uv 0.13.10 or newer, but this sb release provides 0.13.4; run sb self-update"
        );
        assert!(check_uv_minimum("1.0.0", "0.99.99").is_err());
    }

    #[test]
    fn a_version_semver_refuses_is_named() {
        let err = check_uv_minimum("0.13.04", "0.13.4").expect_err("leading zero");
        assert!(
            err.to_string().starts_with("invalid minimum version: "),
            "{err}"
        );
        let err = check_uv_minimum("0.13.4", "x").expect_err("bad actual");
        assert!(
            err.to_string().starts_with("invalid actual version: "),
            "{err}"
        );
    }

    #[test]
    fn the_lock_digest_is_its_sha256() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lock.txt");
        std::fs::write(&path, "abc").expect("write");
        assert_eq!(
            file_sha256(&path).expect("digest"),
            // The file's bytes, hashed by sha2 itself.
            hex::encode(Sha256::digest(b"abc"))
        );
        // Longer than one block.
        std::fs::write(&path, vec![b'a'; 200_000]).expect("write");
        let digest = file_sha256(&path).expect("digest");
        assert_eq!(digest, hex::encode(Sha256::digest(vec![b'a'; 200_000])));
    }
}
