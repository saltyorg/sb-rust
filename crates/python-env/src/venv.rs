//! The venv's health (python-env spec, step 4; sb-go's `python/venv.go`,
//! `validatePython`, `validateEnvironment` and `validateEntrypoints`): the
//! Python check and the environment check, each probe an isolated child
//! captured within 30 s (spec, difference 8).

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use sb_cli_core::exit::Exit;
use sb_host::OwnerError;
use sb_host::apt::PackageQueryError;
use sb_term_ui::{Labels, Step};
use tokio_util::sync::CancellationToken;

use crate::activate::{self, CleanupError};
use crate::isolate::{IsolateError, Kind};
use crate::manifest::{self, Manifest, ManifestError};
use crate::toolchain::{self, Toolchain, ToolchainError};
use crate::wrappers::{self, WrapperError};
use crate::{Env, Layout, captured as probe, uv};

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

/// Which rebuild a command asks for (spec, decision 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Force {
    /// The normal path: only what is unhealthy is rebuilt.
    None,
    /// `reinstall-venv`: a new venv on the reused Python.
    Venv,
    /// `reinstall-python`: a new Python release and venv.
    Python,
}

/// What [`reconcile_venv`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VenvOutcome {
    /// The active venv was healthy and kept; its wrappers were rewritten.
    UpToDate,
}

/// The active venv as found (sb-go's `activeState`).
#[derive(Debug, Default)]
pub(crate) struct Active {
    /// The active generation's manifest, when it holds every field.
    pub(crate) manifest: Option<Manifest>,
    /// Whether it passes every check (spec, step 4).
    pub(crate) healthy: bool,
}

/// Why the active venv couldn't be inspected, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum InspectError {
    /// The active path couldn't be looked at.
    #[error("{}: {cause}", path.display())]
    Io { path: PathBuf, cause: io::Error },
    /// The active path is a file or some other thing.
    #[error("{} is neither a directory nor a symlink", .0.display())]
    Other(PathBuf),
    /// The generation's manifest couldn't be read.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

/// The active venv (sb-go's `inspectActive`): missing or a dangling
/// link, unhealthy; a real directory (the legacy layout, which
/// activation adopts), unhealthy; a symlink, its generation's manifest,
/// healthy when the schema is 1, the Python and the lock match the
/// toolchain and the environment check passes. Why a check failed goes to
/// the run log.
pub(crate) async fn inspect(
    env: &Env<'_>,
    toolchain: &Toolchain,
    token: &CancellationToken,
) -> Result<Active, InspectError> {
    let active = &env.layout.active_venv;
    let failed = |cause| InspectError::Io {
        path: active.clone(),
        cause,
    };
    let meta = match std::fs::symlink_metadata(active) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Active::default()),
        Err(err) => return Err(failed(err)),
        Ok(meta) => meta,
    };
    if meta.is_dir() {
        tracing::info!("the active venv is a legacy directory");
        return Ok(Active::default());
    }
    if !meta.file_type().is_symlink() {
        return Err(InspectError::Other(active.clone()));
    }
    // Go's `filepath.EvalSymlinks` (sb-go): the whole path resolved.
    let target = match std::fs::canonicalize(active) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Active::default()),
        Err(err) => return Err(failed(err)),
        Ok(target) => target,
    };
    let generation = target.parent().unwrap_or(&target);
    let Some(manifest) = manifest::read(generation)? else {
        tracing::info!("the active manifest is incomplete");
        return Ok(Active::default());
    };
    let matches = manifest.schema_version == manifest::SCHEMA
        && manifest.python_version == toolchain.python
        && manifest.lock_sha256 == toolchain.lock_sha256;
    let healthy = if matches {
        match check_environment(env, &target, &toolchain.python, token).await {
            Ok(()) => true,
            Err(err) => {
                tracing::info!(cause = %err, "the active venv is unhealthy");
                false
            }
        }
    } else {
        tracing::info!("the active venv was built for another Python or lock");
        false
    };
    Ok(Active {
        manifest: Some(manifest),
        healthy,
    })
}

/// Why the venv couldn't be reconciled, in sb-go's words. A nested step
/// shows its own failure; it ends the reconcile as [`VenvError::Stopped`].
#[derive(Debug, thiserror::Error)]
pub enum VenvError {
    #[error(transparent)]
    Toolchain(#[from] ToolchainError),
    // The causes below are part of the text, not a `source` as well: the
    // failure block shows the whole chain, which would print them twice
    // (as sb-host's `OwnerError`).
    #[error("check native Python build prerequisites: {0}")]
    Prerequisites(PackageQueryError),
    #[error("inspect active Ansible environment: {0}")]
    Inspect(InspectError),
    #[error("refresh Ansible command wrappers: {0}")]
    Wrappers(WrapperError),
    #[error("clean old Ansible generations: {0}")]
    Cleanup(CleanupError),
    #[error(transparent)]
    Ownership(#[from] OwnerError),
    /// The ownership walk's blocking task panicked or was cancelled.
    #[error("set ownership: {0}")]
    Join(tokio::task::JoinError),
    /// The active venv needs a rebuild, which the build path (module
    /// build M15 and M16) brings.
    #[error("the Ansible virtual environment needs a rebuild, which this sb can't do yet")]
    Rebuild,
    /// A nested step failed or was interrupted; it says why itself.
    #[error("a step stopped the Ansible virtual environment's update")]
    Stopped(#[from] Exit),
}

/// The status step's done label (sb-go's `environmentStatusTaskSpec`).
pub(crate) fn status_label(healthy: bool, force: Force) -> &'static str {
    match force {
        Force::Python => "Python and Ansible virtual environment recreation requested",
        Force::Venv => "Ansible virtual environment recreation requested",
        Force::None if healthy => "Ansible virtual environment was already up to date",
        Force::None => "Ansible virtual environment update required",
    }
}

/// Bring the active venv in line with the checkout (spec, "The venv
/// reconcile", sb-go's `Reconcile`): the toolchain; "Ensuring uv <v> is
/// installed"; libpq-dev, installed when missing; the active venv
/// inspected; "Checking Ansible virtual environment for updates" with
/// sb-go's done label; on a healthy venv and no force, the wrappers
/// rewritten, old generations cleaned and ownership given to `user`
/// (skipped when `None`). So far that is all (module build M14): an
/// unhealthy venv or a forced rebuild is [`VenvError::Rebuild`].
///
/// # Errors
///
/// [`VenvError`]: the first part that fails, a nested step's failure as
/// [`VenvError::Stopped`].
pub async fn reconcile_venv(
    step: &mut Step,
    env: &Env<'_>,
    user: Option<&str>,
    force: Force,
) -> Result<VenvOutcome, VenvError> {
    let token = step.token();
    let toolchain = toolchain::read_toolchain(env, &token).await?;
    let ensuring = format!("Ensuring uv {} is installed", env.uv_version);
    step.step(Labels::new(&ensuring, &ensuring), async |step| {
        uv::check_installed(env, &step.token())
            .await
            .context("install pinned uv")
    })
    .await?;
    prerequisites(step, env, &token).await?;
    let active = inspect(env, &toolchain, &token)
        .await
        .map_err(VenvError::Inspect)?;
    let checking = "Checking Ansible virtual environment for updates";
    let labels = Labels::new(checking, status_label(active.healthy, force));
    step.step(labels, async |_| anyhow::Ok(())).await?;
    let Active {
        manifest: Some(manifest),
        healthy: true,
    } = active
    else {
        return Err(VenvError::Rebuild);
    };
    if force != Force::None {
        return Err(VenvError::Rebuild);
    }
    wrappers::write_all(env.layout, &manifest.exported_commands).map_err(VenvError::Wrappers)?;
    activate::cleanup_generations(env.layout).map_err(VenvError::Cleanup)?;
    ensure_ownership(env.layout, user).await?;
    Ok(VenvOutcome::UpToDate)
}

/// libpq-dev, which psycopg's build needs (spec, step 3; sb-go's
/// `IsPackageInstalled`, `UpdatePackageLists`, `InstallPackage`): when
/// dpkg hasn't it installed, "Updating apt package cache" and
/// "Installing native Python build prerequisites".
async fn prerequisites(
    step: &mut Step,
    env: &Env<'_>,
    token: &CancellationToken,
) -> Result<(), VenvError> {
    let installed = env
        .apt
        .package_installed(LIBPQ, token)
        .await
        .map_err(VenvError::Prerequisites)?;
    if installed {
        return Ok(());
    }
    let updating = "Updating apt package cache";
    step.step(Labels::new(updating, updating), async |step| {
        env.apt
            .update_lists(step)
            .await
            .context("update apt cache for native Python build prerequisites")
    })
    .await?;
    let installing = "Installing native Python build prerequisites";
    step.step(Labels::new(installing, installing), async |step| {
        env.apt
            .install(step, &[LIBPQ])
            .await
            .context("install native Python build prerequisites")
    })
    .await?;
    Ok(())
}

/// The package psycopg's build needs.
const LIBPQ: &str = "libpq-dev";

/// The Saltbox user's ownership (spec, step 16; sb-go's
/// `ensureManagedOwnership`): `/srv/git` itself, then the checkout,
/// `/srv/ansible` and `/srv/python` whole, on a blocking thread (the walk
/// covers thousands of entries:
/// <https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
/// Nothing when `user` is `None` (gha).
async fn ensure_ownership(layout: &Layout, user: Option<&str>) -> Result<(), VenvError> {
    let Some(user) = user else {
        return Ok(());
    };
    let user = user.to_string();
    let single = [layout.git_dir.clone()];
    let trees = [
        layout.checkout.clone(),
        layout.ansible.clone(),
        layout.python.clone(),
    ];
    tokio::task::spawn_blocking(move || {
        sb_host::ensure_owner(&user, &single)?;
        sb_host::ensure_owner_recursive(&user, &trees)
    })
    .await
    .map_err(VenvError::Join)??;
    Ok(())
}
