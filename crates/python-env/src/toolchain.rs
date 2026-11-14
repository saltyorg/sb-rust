//! The toolchain Saltbox pins (python-env spec, "The toolchain"; sb-go's
//! `python/toolchain.go` and `venv.go:76-104`): `.python-version`,
//! `.uv-version` as a minimum against the uv this sb provides, the hashed
//! lock's sha256 and the checkout's commit.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_exec::Cmd;
use tokio_util::sync::CancellationToken;

use crate::Env;

/// `git rev-parse HEAD`'s limit (spec, difference 8).
const COMMIT_TIMEOUT: Duration = Duration::from_secs(10);

/// What the checkout pins, read before anything changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolchain {
    /// `.python-version`: `major.minor.patch`.
    pub python: String,
    /// `.uv-version`: the oldest uv Saltbox accepts.
    pub minimum_uv: String,
    /// The lock's sha256, lower-case hex.
    pub lock_sha256: String,
    /// The checkout's `HEAD`.
    pub commit: String,
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
/// `readExactVersion`), trimmed.
///
/// # Errors
///
/// [`ToolchainError::Read`] or [`ToolchainError::Malformed`].
pub fn read_pin(path: &Path, name: &'static str) -> Result<String, ToolchainError> {
    let text = std::fs::read_to_string(path).map_err(|cause| ToolchainError::Read {
        name,
        path: path.to_path_buf(),
        cause,
    })?;
    let value = text.trim();
    if exact(value) {
        Ok(value.to_string())
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
/// files, the uv minimum against `env.uv_version`, the lock's digest and
/// `git rev-parse HEAD` in the checkout (capture, 10 s).
///
/// # Errors
///
/// [`ToolchainError`]: the first pin, minimum, lock or commit failure.
pub async fn read_toolchain(
    env: &Env<'_>,
    token: &CancellationToken,
) -> Result<Toolchain, ToolchainError> {
    let layout = env.layout;
    let python = read_pin(&layout.python_pin, "Python")?;
    let minimum_uv = read_pin(&layout.uv_pin, "uv")?;
    check_uv_minimum(&minimum_uv, env.uv_version)?;
    let lock_sha256 = file_sha256(&layout.lock).map_err(ToolchainError::Lock)?;
    let commit = commit(env, token).await?;
    Ok(Toolchain {
        python,
        minimum_uv,
        lock_sha256,
        commit,
    })
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
        assert_eq!(read_pin(&path, "Python").expect("pin"), "3.12.7");
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

    #[test]
    fn a_missing_pin_names_its_file() {
        let path = Path::new("/nonexistent/.python-version");
        let err = read_pin(path, "Python").expect_err("missing");
        assert!(
            err.to_string().starts_with(
                "load Saltbox Python toolchain: read Python version from /nonexistent/.python-version: "
            ),
            "{err}"
        );
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
