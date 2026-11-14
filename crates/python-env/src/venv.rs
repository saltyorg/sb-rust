//! The venv's health (python-env spec, step 4; sb-go's `python/venv.go`,
//! `validatePython`, `validateEnvironment` and `validateEntrypoints`): the
//! Python check and the environment check, each probe an isolated child
//! captured within 30 s (spec, difference 8).

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::isolate::{IsolateError, Kind};
use crate::{Env, captured as probe};

/// Each health probe's limit (spec, difference 8).
const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

/// The wider Python check (spec, decision 3): the standard library and
/// the native modules a broken build loses first. sb-go imports
/// `encodings` only.
const STDLIB: &str = "import encodings, ssl, sqlite3, ctypes";

/// The packages Saltbox's playbooks need (sb-go's import check).
const PACKAGES: &str = "import ansible, apprise, certbot";

/// The entry points each venv must run, in sb-go's order.
pub(crate) const ENTRY_POINTS: [&str; 3] = ["ansible", "certbot", "apprise"];

/// Why a Python or a venv isn't healthy, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// A probe's child couldn't be prepared.
    #[error(transparent)]
    Isolate(#[from] IsolateError),
    /// The interpreter couldn't run `--version`.
    #[error("run Python at {}: {cause}", path.display())]
    RunPython { path: PathBuf, cause: String },
    /// The interpreter is another release.
    #[error("python at {} reports {got:?}, expected {want:?}", path.display())]
    PythonVersion {
        path: PathBuf,
        got: String,
        want: String,
    },
    /// The interpreter can't import its standard library.
    #[error("python at {} cannot import its standard library: {cause}", path.display())]
    Stdlib { path: PathBuf, cause: String },
    /// `uv pip check` failed.
    #[error("installed Python packages are incompatible: {0}")]
    Packages(String),
    /// Saltbox's packages don't import.
    #[error("import Saltbox Python packages: {0}")]
    Imports(String),
    /// An entry point doesn't run `--version`.
    #[error("run {name} health check: {cause}")]
    EntryPoint { name: &'static str, cause: String },
}

/// The Python check (sb-go's `validatePython`, with decision 3's wider
/// import): `python -I --version` is `Python <version>`, then the
/// standard library imports.
///
/// # Errors
///
/// [`CheckError`]: the first probe that fails.
pub async fn check_python(
    env: &Env<'_>,
    python: &Path,
    version: &str,
    token: &CancellationToken,
) -> Result<(), CheckError> {
    let reported = env
        .isolated(Kind::Python, python)?
        .map(|cmd| cmd.arg("--version").timeout(HEALTH_TIMEOUT));
    let got = probe(&reported.cmd, token)
        .await
        .map_err(|cause| CheckError::RunPython {
            path: python.to_path_buf(),
            cause,
        })?;
    let want = format!("Python {version}");
    let got = got.trim();
    if got != want {
        return Err(CheckError::PythonVersion {
            path: python.to_path_buf(),
            got: got.to_string(),
            want,
        });
    }
    let imports = env
        .isolated(Kind::Python, python)?
        .map(|cmd| cmd.arg("-c").arg(STDLIB).timeout(HEALTH_TIMEOUT));
    probe(&imports.cmd, token)
        .await
        .map_err(|cause| CheckError::Stdlib {
            path: python.to_path_buf(),
            cause,
        })?;
    Ok(())
}

/// The environment check of the venv at `venv` (sb-go's
/// `validateEnvironment`): its `bin/python3` passes the Python check,
/// `uv pip check` passes, Saltbox's packages import, and `ansible`,
/// `certbot` and `apprise` each run `--version` in the venv (`ansible`
/// with no `ANSIBLE_*` but an empty config).
///
/// # Errors
///
/// [`CheckError`]: the first probe that fails.
pub async fn check_environment(
    env: &Env<'_>,
    venv: &Path,
    version: &str,
    token: &CancellationToken,
) -> Result<(), CheckError> {
    let python = venv.join("bin/python3");
    check_python(env, &python, version, token).await?;
    let python_arg = python.to_string_lossy().into_owned();
    let pip_check = env
        .isolated(Kind::Uv { cache: true }, &env.layout.uv)?
        .map(|cmd| {
            cmd.arg("pip")
                .arg("check")
                .arg("--python")
                .value(python_arg)
                .timeout(HEALTH_TIMEOUT)
        });
    probe(&pip_check.cmd, token)
        .await
        .map_err(CheckError::Packages)?;
    let imports = env
        .isolated(Kind::Python, &python)?
        .map(|cmd| cmd.arg("-c").arg(PACKAGES).timeout(HEALTH_TIMEOUT));
    probe(&imports.cmd, token)
        .await
        .map_err(CheckError::Imports)?;
    for name in ENTRY_POINTS {
        let kind = Kind::Entrypoint {
            venv,
            ansible: name == "ansible",
        };
        let program = venv.join("bin").join(name);
        let check = env
            .isolated(kind, &program)?
            .map(|cmd| cmd.arg("--version").timeout(HEALTH_TIMEOUT));
        probe(&check.cmd, token)
            .await
            .map_err(|cause| CheckError::EntryPoint { name, cause })?;
    }
    Ok(())
}
