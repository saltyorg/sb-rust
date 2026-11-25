//! The venv (python-env spec, "The venv reconcile"; sb-go's
//! `python/venv.go`): the Python check and the environment check (step 4,
//! each probe an isolated child captured within 30 s, difference 8), the
//! reconcile, the build into staging (steps 7-11 and 17) and its
//! activation (steps 12-16).

use std::io;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use sb_cli_core::exit::{Exit, escaped, shown_chain};
use sb_exec::Cmd;
use sb_host::OwnerError;
use sb_term_ui::{Labels, Step};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::activate::{self, ActivateError, CleanupError};
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

/// A probe's ending: its output, or exec's text for its failure; or why
/// its child couldn't be prepared.
type Ran = Result<Result<String, String>, IsolateError>;

/// The turns the probes take, [`Env::probes_at_once`] at a time (one per
/// physical core sb may run on, by default; noted in the run log). Each
/// check's `tokio::join!` is `biased`, so its probes ask in the check's
/// order, and tokio's semaphore gives turns in the order asked
/// (<https://docs.rs/tokio/1.54.1/tokio/sync/struct.Semaphore.html>):
/// every probe still waiting comes after every probe that has had its
/// turn.
fn turns(env: &Env<'_>) -> Semaphore {
    let at_once = env
        .probes_at_once
        .unwrap_or_else(sb_host::cores::children_at_once);
    tracing::info!(at_once, "the environment check's probes take turns");
    Semaphore::new(at_once.get())
}

/// One probe, in its turn (held to its end): `program`'s child prepared
/// as `kind` with `args` applied and [`HEALTH_TIMEOUT`], then captured.
/// The child's scratch directory lives until it ends.
///
/// A probe that fails closes `turns`, and the probes still waiting then
/// don't run, as the check stopped at its first failure when the probes
/// ran in turn: they all come after it, so its failure is the one named
/// (a hung Python costs one timeout, not one for each probe). Ctrl-C
/// fails the running probes, so the waiting ones don't start either, and
/// a probe whose turn comes after it doesn't start its child. The
/// placeholder text of a probe that didn't run is never shown: the
/// failure before it is named, or the caller reports the interruption.
async fn run_probe(
    env: &Env<'_>,
    turns: &Semaphore,
    kind: Kind<'_>,
    program: impl AsRef<Path>,
    args: impl FnOnce(Cmd) -> Cmd,
    token: &CancellationToken,
) -> Ran {
    let Ok(_turn) = turns.acquire().await else {
        return Ok(Err("not run: an earlier probe failed".to_string()));
    };
    // Ctrl-C before this probe's turn: it doesn't start. exec's `capture`
    // starts its child before it looks at the token, and a child stopped
    // at once may still have begun its work.
    if token.is_cancelled() {
        turns.close();
        return Ok(Err("not run: interrupted".to_string()));
    }
    let ran = env
        .isolated(kind, program.as_ref())
        .map(|isolated| isolated.map(|cmd| args(cmd).timeout(HEALTH_TIMEOUT)));
    let ran = match ran {
        // Boxed: seven captures side by side would make the check's
        // future (and every caller's) about 18 KB (clippy's
        // `large_futures`).
        Ok(prepared) => Ok(Box::pin(probe(&prepared.cmd, token)).await),
        Err(err) => Err(err),
    };
    if !matches!(ran, Ok(Ok(_))) {
        turns.close();
    }
    ran
}

/// The Python check's two probes: `python -I --version`, and the
/// standard library's imports.
fn python_probes<'a>(
    env: &'a Env<'_>,
    turns: &'a Semaphore,
    python: &'a Path,
    token: &'a CancellationToken,
) -> (
    impl Future<Output = Ran> + 'a,
    impl Future<Output = Ran> + 'a,
) {
    let run = |args: &'static [&'static str]| {
        run_probe(
            env,
            turns,
            Kind::Python,
            python,
            move |cmd| args.iter().fold(cmd, |cmd, arg| cmd.arg(arg)),
            token,
        )
    };
    (run(&["--version"]), run(&["-c", STDLIB]))
}

/// The Python check's verdict on its probes' endings, the first failure
/// in sb-go's order: `--version` ran and reported `Python <version>`,
/// then the standard library imported.
fn python_checked(
    python: &Path,
    version: &str,
    reported: Ran,
    stdlib: Ran,
) -> Result<(), CheckError> {
    let got = reported?.map_err(|cause| CheckError::RunPython {
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
    stdlib?.map_err(|cause| CheckError::Stdlib {
        path: python.to_path_buf(),
        cause,
    })?;
    Ok(())
}

/// The Python check (sb-go's `validatePython`, with decision 3's wider
/// import): `python -I --version` is `Python <version>`, then the
/// standard library imports. The two probes run at once where sb may use
/// two CPUs (as [`check_environment`]'s); the failure returned is the
/// first in that order, as when they ran one after the other.
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
    let turns = turns(env);
    let (reported, stdlib) = python_probes(env, &turns, python, token);
    let (reported, stdlib) = tokio::join!(biased; reported, stdlib);
    python_checked(python, version, reported, stdlib)
}

/// The environment check of the venv at `venv` (sb-go's
/// `validateEnvironment`): its `bin/python3` passes the Python check,
/// `uv pip check` passes, Saltbox's packages import, and `ansible`,
/// `certbot` and `apprise` each run `--version` in the venv (`ansible`
/// with no `ANSIBLE_*` but an empty config).
///
/// The seven probes run at once, as many at a time as sb may use
/// physical cores ([`Env::probes_at_once`]; `tokio::join!`,
/// <https://docs.rs/tokio/1.54.1/tokio/macro.join.html>): one after the
/// other they took about a second on every update, the longest
/// (`ansible --version`) about a third of it (performance research,
/// round 2, item 3), and on one core (SMT siblings included) they still
/// run in turn, as more Pythons than cores cost more CPU time than the
/// wait they save. After a
/// failure the probes not yet started don't run ([`run_probe`]). Each is
/// independent of the others' results, and the failure returned is the
/// first in the order above, so the error text is the one sb-go's
/// sequence gives.
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
    let python_arg = python.to_string_lossy().into_owned();
    let turns = turns(env);
    let (reported, stdlib) = python_probes(env, &turns, &python, token);
    let pip_check = run_probe(
        env,
        &turns,
        Kind::Uv { cache: true },
        &env.layout.uv,
        |cmd| {
            cmd.arg("pip")
                .arg("check")
                .arg("--python")
                .shown_value(python_arg)
        },
        token,
    );
    let imports = run_probe(
        env,
        &turns,
        Kind::Python,
        &python,
        |cmd| cmd.arg("-c").arg(PACKAGES),
        token,
    );
    let [first, second, third] = ENTRY_POINTS.map(|name| {
        let kind = Kind::Entrypoint {
            venv,
            ansible: name == "ansible",
        };
        let program = venv.join("bin").join(name);
        run_probe(
            env,
            &turns,
            kind,
            program,
            |cmd| cmd.arg("--version"),
            token,
        )
    });
    let (reported, stdlib, pip_check, imports, first, second, third) = tokio::join!(
        biased;
        reported, stdlib, pip_check, imports, first, second, third
    );
    python_checked(&python, version, reported, stdlib)?;
    pip_check?.map_err(CheckError::Packages)?;
    imports?.map_err(CheckError::Imports)?;
    for (name, ran) in ENTRY_POINTS.into_iter().zip([first, second, third]) {
        ran?.map_err(|cause| CheckError::EntryPoint { name, cause })?;
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
    /// A new generation was built and activated.
    Rebuilt,
    /// The first build failed; the clean retry's generation was built and
    /// activated (spec, decision 3). A rebuild as well.
    Retried,
}

/// The active venv as found (sb-go's `activeState`).
#[derive(Debug, Default)]
pub(crate) struct Active {
    /// The active generation's manifest, when it holds every field.
    pub(crate) manifest: Option<Manifest>,
    /// Whether it passes every check (spec, step 4).
    pub(crate) healthy: bool,
    /// Its venv, the active link resolved, when it has a manifest.
    pub(crate) venv: Option<PathBuf>,
}

/// Why the active venv couldn't be inspected, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum InspectError {
    /// The active path couldn't be looked at.
    #[error("{}: {cause}", path.display())]
    Io { path: PathBuf, cause: io::Error },
    /// Ctrl-C (or a stop) came during the environment check's probes.
    #[error("interrupted while checking the active Ansible environment")]
    Interrupted,
    /// The generation's manifest couldn't be read for a reason other than
    /// its absence or its text (those make the venv unhealthy).
    #[error(transparent)]
    Manifest(#[from] ManifestError),
}

/// The active venv (sb-go's `inspectActive`): missing or a dangling
/// link, unhealthy; a real directory (the legacy layout, which
/// activation adopts), unhealthy; a file or any other non-link,
/// unhealthy (activation replaces it; sb-go fails); a symlink, its
/// generation's manifest, healthy when the schema is 1, the Python and
/// the lock match the toolchain and the environment check passes. A
/// manifest that is missing or isn't JSON of the right shape makes it
/// unhealthy too (sb-go fails; spec, difference 17). Why a check failed
/// goes to the run log. Under a `force` the environment check doesn't
/// run and the venv counts as unhealthy: the rebuild replaces it whatever
/// the check would say, and its seven probes are about a second of CPU
/// (performance research, round 5; sb-go runs them first,
/// `python/venv.go:126`).
pub(crate) async fn inspect(
    env: &Env<'_>,
    toolchain: &Toolchain,
    force: Force,
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
        tracing::info!("the active venv path is neither a directory nor a symlink");
        return Ok(Active::default());
    }
    // Go's `filepath.EvalSymlinks` (sb-go): the whole path resolved.
    let target = match std::fs::canonicalize(active) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Active::default()),
        Err(err) => return Err(failed(err)),
        Ok(target) => target,
    };
    let generation = target.parent().unwrap_or(&target);
    let read = match manifest::read(generation) {
        Err(ManifestError::Read { cause, .. }) if cause.kind() == io::ErrorKind::NotFound => {
            tracing::info!(generation = %generation.display(), "the active manifest is missing");
            return Ok(Active::default());
        }
        Err(err @ ManifestError::Parse { .. }) => {
            tracing::info!(cause = %err, "the active manifest is unreadable");
            return Ok(Active::default());
        }
        read => read?,
    };
    let Some(manifest) = read else {
        tracing::info!("the active manifest is incomplete");
        return Ok(Active::default());
    };
    // The install's kind too (sb-go 69b968b's `manifestMatches`): moving
    // between a pinned and a pin-less checkout rebuilds the venv even
    // when the Python and the lock's bytes are the same.
    let matches = manifest.schema_version == manifest::SCHEMA
        && manifest.python_version == toolchain.python
        && manifest.lock_sha256 == toolchain.lock_sha256
        && manifest.requirements_unhashed != toolchain.hashed;
    let healthy = if matches && force != Force::None {
        tracing::info!(
            ?force,
            "the active venv isn't checked: the rebuild replaces it"
        );
        false
    } else if matches {
        match check_environment(env, &target, &toolchain.python, token).await {
            Ok(()) => true,
            // A probe Ctrl-C ended says nothing of the venv: the
            // inspection's step ends interrupted, not done.
            Err(_) if token.is_cancelled() => return Err(InspectError::Interrupted),
            Err(err) => {
                tracing::info!(cause = %err, "the active venv is unhealthy");
                false
            }
        }
    } else {
        tracing::info!(
            "the active venv was built for another Python or lock, or another kind of install"
        );
        false
    };
    Ok(Active {
        manifest: Some(manifest),
        healthy,
        venv: Some(target),
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
    /// Shown in the inspection's own step; the reconcile then ends as
    /// [`VenvError::Stopped`].
    #[error("inspect active Ansible environment: {0}")]
    Inspect(InspectError),
    #[error("refresh Ansible command wrappers: {0}")]
    Wrappers(WrapperError),
    /// The new generation's wrappers couldn't be written.
    #[error("install Ansible command wrappers: {0}")]
    InstallWrappers(WrapperError),
    #[error("remove stale Ansible command wrappers: {0}")]
    StaleWrappers(WrapperError),
    #[error("clean old Ansible generations: {0}")]
    Cleanup(CleanupError),
    #[error(transparent)]
    Ownership(#[from] OwnerError),
    /// The ownership walk's blocking task panicked or was cancelled.
    #[error("set ownership: {0}")]
    Join(tokio::task::JoinError),
    /// The cleanup's blocking task panicked or was cancelled.
    #[error("clean old Ansible generations: {0}")]
    CleanupJoin(tokio::task::JoinError),
    /// The build failed; what it made is gone. A nested step's quiet
    /// exit stays in the chain (`transparent` forwards `source`).
    #[error(transparent)]
    Build(#[from] BuildError),
    /// The new generation couldn't be activated; the active venv is as
    /// it was.
    #[error(transparent)]
    Activate(ActivateError),
    /// Ctrl-C came after the build, before activation: the build is
    /// removed and the active venv kept (spec, step 17).
    #[error("interrupted before activating the new Ansible virtual environment")]
    Interrupted,
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
/// reconcile", sb-go's `Reconcile`): the pins; "Ensuring uv <v> is
/// installed"; the Python resolved against that uv's list, with a
/// warning when the checkout pins none; "Inspecting the active Ansible virtual environment";
/// "Checking Ansible virtual environment for updates" with sb-go's done
/// label. On a healthy venv and no force, the wrappers
/// rewritten, old generations cleaned and ownership given to `user`
/// (skipped when `None`). Otherwise a new generation is built into
/// staging ([`build`]; a failed build on the normal path is retried
/// once as a `Python` force, a fresh Python and no uv cache), its
/// wrappers written, activated (a legacy directory adopted), the
/// previous generation's stale wrappers removed, then the steps
/// "Cleaning up old environments" (old generations cleaned) and,
/// with a user, "Setting ownership". A failure or Ctrl-C
/// before activation removes the build and leaves the active venv as it
/// was (spec, step 17).
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
    let pins = toolchain::read_pins(env, &token).await?;
    let ensuring = format!("Ensuring uv {} is installed", env.uv_version);
    step.step(Labels::new(&ensuring, &ensuring), async |step| {
        uv::ensure_installed(step, env)
            .await
            .context("install pinned uv")
    })
    .await?;
    // After the uv is in place, which the resolution asks (sb-go's order).
    let toolchain = toolchain::resolve(env, pins, &token).await?;
    if let Some(notice) = toolchain.notice(env.uv_version) {
        step.warn(notice);
    }
    // Its probes take about a second on a Saltbox venv: a step of its
    // own, so the run doesn't sit silent before the next one (spec, step 4).
    let inspecting = Labels::new(
        "Inspecting the active Ansible virtual environment",
        "Active Ansible virtual environment inspected",
    );
    let active = step
        .step(inspecting, async |step| {
            inspect(env, &toolchain, force, &step.token())
                .await
                .map_err(|err| anyhow::Error::from(VenvError::Inspect(err)))
        })
        .await?;
    let damage = if env.verify {
        verify_files(step, env, &active).await?
    } else {
        Damage::default()
    };
    let healthy = active.healthy && !damage.any();
    let checking = "Checking Ansible virtual environment for updates";
    let labels = Labels::new(checking, status_label(healthy, force));
    step.step(labels, async |_| anyhow::Ok(())).await?;
    if let Some(manifest) = &active.manifest
        && healthy
        && force == Force::None
    {
        wrappers::write_all(env.layout, &manifest.exported_commands)
            .map_err(VenvError::Wrappers)?;
        cleanup(env.layout).await?;
        ensure_ownership(env.layout, user).await?;
        return Ok(VenvOutcome::UpToDate);
    }
    let previous = active.manifest;
    let build_force = damage.raise(force);
    let (staged, outcome) = match build(step, env, &toolchain, previous.as_ref(), build_force).await
    {
        Ok(staged) => (staged, VenvOutcome::Rebuilt),
        Err(err) if retries(force, token.is_cancelled()) => {
            // The run log keeps the first cause: in this line, and in the
            // failed step's record when a nested step failed. A second
            // failure fails the step with its own cause.
            step.info(format!(
                "Retrying with a fresh Python and no uv cache after: {}",
                err.cause()
            ));
            let staged = build(step, env, &toolchain, previous.as_ref(), Force::Python).await?;
            (staged, VenvOutcome::Retried)
        }
        Err(err) => return Err(err.into()),
    };
    let current = activate_staged(env, staged, &token).await?;
    let stale = previous.map(|previous| previous.exported_commands);
    wrappers::remove_stale(
        env.layout,
        stale.as_deref().unwrap_or_default(),
        &current.exported_commands,
    )
    .map_err(VenvError::StaleWrappers)?;
    finish_steps(step, env.layout, user).await?;
    Ok(outcome)
}

/// After a rebuild, the generation cleanup and the ownership pass as
/// sub-steps of their own (spec, steps 15 and 16): both block for a
/// while (a whole venv removed, every entry of the managed trees looked
/// at), and as steps they are drawn, timed and logged as the build's
/// are. Without a user (gha), no ownership step.
async fn finish_steps(
    step: &mut Step,
    layout: &Layout,
    user: Option<&str>,
) -> Result<(), VenvError> {
    let removing = Labels::new(
        "Cleaning up old environments",
        "Old environments cleaned up",
    );
    step.step(removing, async |_| {
        cleanup(layout).await.map_err(anyhow::Error::from)
    })
    .await?;
    if let Some(user) = user {
        let owning = Labels::new("Setting ownership", "Ownership set");
        step.step(owning, async |_| {
            ensure_ownership(layout, Some(user))
                .await
                .map_err(anyhow::Error::from)
        })
        .await?;
    }
    Ok(())
}

/// What `--verify-venv` found damaged (spec, decision 25).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Damage {
    /// A package file: the venv is rebuilt.
    packages: bool,
    /// A file of its Python release: the Python is rebuilt too.
    python: bool,
}

impl Damage {
    fn any(self) -> bool {
        self.packages || self.python
    }

    /// The build that repairs it: a damaged Python a `Python` force, a
    /// damaged package at least a `Venv` one; both clear uv's cache
    /// first, whose hardlinks may share the damage (step 7).
    fn raise(self, force: Force) -> Force {
        if self.python {
            Force::Python
        } else if self.packages && force == Force::None {
            Force::Venv
        } else {
            force
        }
    }
}

/// How many bad and how many missing files the run log names, each; the
/// counts are always there.
const NAMED: usize = 100;

/// "Verifying Ansible virtual environment files" (spec, decision 25): the
/// active venv's files against their RECORDs and its Python release's
/// against sb's manifest, on the blocking pool, the counts shown and the
/// files named in the run log. Nothing without an active manifest (the
/// venv is rebuilt anyway).
async fn verify_files(
    step: &mut Step,
    env: &Env<'_>,
    active: &Active,
) -> Result<Damage, VenvError> {
    let (Some(manifest), Some(venv)) = (&active.manifest, &active.venv) else {
        return Ok(Damage::default());
    };
    let labels = Labels::new(
        "Verifying Ansible virtual environment files",
        "Ansible virtual environment files verified",
    );
    let damage = step
        .step(labels, async |step| {
            let token = step.token();
            let venv = venv.clone();
            let python = PathBuf::from(&manifest.python_install);
            let managed = activate::direct_child(&python, &env.layout.python_releases);
            let stored = crate::verify::manifest_path(env.layout, &python);
            let owner = sb_cli_core::root::effective_uid();
            let shown = python.clone();
            let (packages, release) = sb_cli_core::spawn::blocking(move || {
                let packages = crate::verify::packages(&venv, &token)?;
                let release = managed
                    .then(|| crate::verify::check_release(&python, &stored, owner, &token))
                    .transpose()?;
                Ok::<_, crate::verify::Interrupted>((packages, release))
            })
            .await??;
            report(step, "Packages", &packages);
            let python = match release {
                Some(crate::verify::ReleaseCheck::Checked(found)) => {
                    report(step, "Python", &found);
                    found.damaged()
                }
                Some(crate::verify::ReleaseCheck::NoManifest) => {
                    step.info(
                        "Python: not checked, no manifest yet (sb writes one when it installs \
                         the Python: sb reinstall-python)",
                    );
                    false
                }
                // The reason and the path come from the user's tree in
                // part: one line, controls escaped (cli-core's rule), so
                // they can't draw a notice of their own.
                Some(crate::verify::ReleaseCheck::Untrusted(why)) => {
                    tracing::warn!(why, "Python release manifest not trusted");
                    step.info(format!("Python: not checked, {}", escaped(&why)));
                    false
                }
                None => {
                    let shown = shown.display().to_string();
                    step.info(format!(
                        "Python: not checked, {} is not a release sb manages",
                        escaped(&shown)
                    ));
                    false
                }
            };
            anyhow::Ok(Damage {
                packages: packages.damaged(),
                python,
            })
        })
        .await?;
    Ok(damage)
}

/// `what`'s counts as a notice, and in the run log with the first
/// [`NAMED`] bad and missing files.
fn report(step: &mut Step, what: &str, found: &crate::verify::Findings) {
    let files = if found.checked == 1 { "file" } else { "files" };
    let conflicting = match found.conflicting.len() {
        0 => String::new(),
        n => format!(", {n} listed differently by two packages (not checked)"),
    };
    step.info(format!(
        "{what}: {} {files} checked, {} bad, {} missing{conflicting}",
        found.checked,
        found.bad.len(),
        found.missing.len()
    ));
    tracing::info!(
        what,
        checked = found.checked,
        bad = found.bad.len(),
        missing = found.missing.len(),
        conflicting = found.conflicting.len(),
        "files verified"
    );
    for path in found.conflicting.iter().take(NAMED) {
        tracing::info!(what, path = %path.display(), "file listed differently by two packages");
    }
    for path in found.bad.iter().take(NAMED) {
        tracing::warn!(what, path = %path.display(), "file differs from its record");
    }
    for path in found.missing.iter().take(NAMED) {
        tracing::warn!(what, path = %path.display(), "file missing");
    }
}

/// Whether a failed build is retried (spec, "Self-healing on the normal
/// path"; decision 3): once, on the normal path only, never after a
/// cancel. Only the build (steps 7-11) gets here: the toolchain, uv, apt
/// and activation fail the reconcile before or after it.
fn retries(force: Force, cancelled: bool) -> bool {
    force == Force::None && !cancelled
}

/// Old generations cleaned ([`activate::cleanup_generations`]) on a
/// blocking thread: after a rebuild it removes a whole venv, thousands of
/// files, and sb's runtime has one thread, which also draws the steps
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
async fn cleanup(layout: &Layout) -> Result<(), VenvError> {
    let layout = layout.clone();
    // The run's span goes along, so the thread's records carry the run id
    // (cli-core's `spawn`).
    sb_cli_core::spawn::blocking(move || activate::cleanup_generations(&layout))
        .await
        .map_err(VenvError::CleanupJoin)?
        .map_err(VenvError::Cleanup)
}

/// The staged generation's wrappers written, then the generation
/// activated (spec, steps 12 and 13; sb-go's `installWrappers` and
/// `activateGeneration`), unless Ctrl-C came first, which leaves both as
/// they were. On any failure the
/// build is removed and the active venv is as it was.
async fn activate_staged(
    env: &Env<'_>,
    staged: Staged<'_>,
    token: &CancellationToken,
) -> Result<Manifest, VenvError> {
    // The last moment a cancel can still keep the active venv and its
    // wrappers: nothing below awaits.
    let activated = if token.is_cancelled() {
        Err(VenvError::Interrupted)
    } else {
        Ok(())
    };
    let activated = activated
        .and_then(|()| {
            wrappers::write_all(env.layout, &staged.manifest.exported_commands)
                .map_err(VenvError::InstallWrappers)
        })
        .and_then(|()| {
            activate::activate_generation(env.layout, &staged.venv).map_err(VenvError::Activate)
        });
    match activated {
        Ok(()) => Ok(staged.keep()),
        Err(err) => {
            staged.discard().await;
            Err(err)
        }
    }
}

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
    // The run's span goes along, so the ownership records carry the run id
    // (cli-core's `spawn`).
    sb_cli_core::spawn::blocking(move || {
        sb_host::ensure_owner(&user, &single)?;
        sb_host::ensure_owner_recursive(&user, &trees)
    })
    .await
    .map_err(VenvError::Join)??;
    Ok(())
}

/// `uv python find`'s limit: a local lookup, as quick as a health probe
/// (sb-go has none; the spec's rule "a timeout or a stated reason for
/// none", and its difference 8).
const FIND_TIMEOUT: Duration = Duration::from_secs(30);

/// Go's `20060102T150405.000000000` in jiff's strftime
/// (<https://docs.rs/jiff/0.2.38/jiff/fmt/strtime/index.html>: `%N` is
/// `%9f`, always nine digits).
const STAMP: &str = "%Y%m%dT%H%M%S.%N";

/// Why the build failed, in sb-go's words. A nested step shows its own
/// failure (install, venv, sync) and ends the build as
/// [`BuildError::Stopped`].
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error(transparent)]
    Isolate(#[from] IsolateError),
    #[error("create Python release directory: {0}")]
    PythonDir(io::Error),
    #[error("error finding Python {version}: {cause}")]
    Find { version: String, cause: String },
    #[error("uv returned non-absolute path {path:?} for Python {version}")]
    NotAbsolute { path: String, version: String },
    #[error(transparent)]
    Python(CheckError),
    #[error("create Ansible generation {id}: {cause}")]
    Generation { id: String, cause: io::Error },
    #[error("read venv commands: {0}")]
    Commands(io::Error),
    #[error("inspect venv command {name}: {cause}")]
    Command { name: String, cause: io::Error },
    #[error("unsafe venv command name {0:?}")]
    UnsafeName(String),
    #[error("required command {0} is missing from staged venv")]
    Missing(&'static str),
    /// Shown in the validation's own step; the build then ends as
    /// [`BuildError::Stopped`], its text kept as the cause.
    #[error("validate staged Ansible environment: {0}")]
    Staged(CheckError),
    #[error(transparent)]
    Manifest(#[from] manifest::WriteError),
    /// A nested step failed or was interrupted; it says why itself, and
    /// `cause` keeps its error's text for the retry's info line.
    #[error("a step stopped the Ansible virtual environment's build")]
    Stopped {
        #[source]
        exit: Exit,
        cause: Option<String>,
    },
}

impl BuildError {
    /// Why the build failed, in one line: a nested step's own error, else
    /// this error's text.
    #[must_use]
    pub(crate) fn cause(&self) -> String {
        match self {
            Self::Stopped {
                cause: Some(cause), ..
            } => cause.clone(),
            other => other.to_string(),
        }
    }
}

/// A build's nested step: `body` under `labels`, its failure's text kept
/// in [`BuildError::Stopped`] (the step shows it; the `Exit` it ends with
/// says only that it failed), withheld past a sensitive cause as the
/// step's own record is (cli-core's `shown_chain`).
async fn nested<T>(
    step: &mut Step,
    labels: Labels,
    body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<T>,
) -> Result<T, BuildError> {
    let mut cause = None;
    let ran = step
        .step(labels, async |step| {
            body(step)
                .await
                .inspect_err(|err| cause = Some(shown_chain(err.as_ref()).join(": ")))
        })
        .await;
    ran.map_err(|exit| BuildError::Stopped { exit, cause })
}

/// What a build has made so far (sb-go's deferred cleanup, venv.go:163-171
/// and 259-264; spec, step 17): the new generation, and the new Python
/// release. A failed build removes them on a blocking thread
/// ([`Leftovers::remove`]: a venv holds thousands of files, and sb's
/// runtime has one thread); dropped while still armed (a panic, a future
/// dropped, a [`Staged`] never kept), they go in `Drop`. Neither removes
/// the generation the active link points at, nor a Python release an
/// active manifest names. Removal is best effort, as sb-go's; a failure
/// goes to the run log. Why not tempfile's `TempDir` for them: it can't
/// make the missing `releases/` parents, drops its removal error unseen,
/// and can't spare a release the active manifest names.
#[derive(Debug)]
struct Leftovers<'a> {
    layout: &'a Layout,
    generation: Option<PathBuf>,
    python: Option<PathBuf>,
    armed: bool,
}

impl Leftovers<'_> {
    /// Remove them now, off the runtime's thread
    /// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>),
    /// and disarm.
    async fn remove(&mut self) {
        self.armed = false;
        let (layout, generation, python) = (
            self.layout.clone(),
            self.generation.take(),
            self.python.take(),
        );
        // The run's span goes along (cli-core's `spawn`).
        let removed = sb_cli_core::spawn::blocking(move || {
            remove_leftovers(&layout, generation.as_deref(), python.as_deref());
        })
        .await;
        if let Err(err) = removed {
            tracing::warn!(%err, "the failed build's removal stopped");
        }
    }
}

impl Drop for Leftovers<'_> {
    fn drop(&mut self) {
        if self.armed {
            remove_leftovers(
                self.layout,
                self.generation.as_deref(),
                self.python.as_deref(),
            );
        }
    }
}

/// A build's `generation` and `python`, unless the active link or
/// manifest still uses them.
fn remove_leftovers(layout: &Layout, generation: Option<&Path>, python: Option<&Path>) {
    let active = std::fs::canonicalize(&layout.active_venv)
        .ok()
        .and_then(|venv| venv.parent().map(Path::to_path_buf));
    if let Some(generation) = generation
        && active.as_deref() != Some(generation)
    {
        remove_tree(generation);
    }
    if let Some(python) = python {
        let named = active
            .and_then(|generation| manifest::read(&generation).ok().flatten())
            .is_some_and(|manifest| Path::new(&manifest.python_install) == python);
        if !named && crate::activate::direct_child(python, &layout.python_releases) {
            remove_tree(python);
            let _gone = std::fs::remove_file(crate::verify::manifest_path(layout, python));
        }
    }
}

/// `path` and everything under it, best effort.
fn remove_tree(path: &Path) {
    if let Err(err) = std::fs::remove_dir_all(path)
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %err, "could not remove a failed build");
    }
}

/// A generation built into staging and checked, its manifest written. It
/// is removed when dropped, unless [`Staged::keep`] took it (activation).
#[derive(Debug)]
#[must_use = "dropping it removes the build"]
pub struct Staged<'a> {
    leftovers: Leftovers<'a>,
    /// The generation directory, holding `venv/` and `manifest.json`.
    pub generation: PathBuf,
    /// Its venv.
    pub venv: PathBuf,
    /// What its manifest holds.
    pub manifest: Manifest,
}

impl Staged<'_> {
    /// Keep the build (it was activated) and return its manifest.
    #[must_use]
    pub fn keep(mut self) -> Manifest {
        self.leftovers.armed = false;
        self.manifest
    }

    /// Remove the build now, off the runtime's thread (dropping it removes
    /// it on the caller's).
    pub async fn discard(mut self) {
        self.leftovers.remove().await;
    }
}

/// The Python a build uses: its interpreter and the release holding it.
#[derive(Debug)]
struct Python {
    path: String,
    install: String,
}

/// The Python the build uses: the active manifest's when the force allows,
/// its version matches and it passes the Python check (spec, step 7),
/// else a new release.
async fn python<'a>(
    step: &mut Step,
    env: &Env<'a>,
    toolchain: &Toolchain,
    previous: Option<&Manifest>,
    force: Force,
    leftovers: &mut Leftovers<'a>,
) -> Result<Python, BuildError> {
    let token = step.token();
    if force != Force::Python
        && let Some(previous) = previous
        && previous.python_version == toolchain.python
    {
        match check_python(
            env,
            Path::new(&previous.python_path),
            &toolchain.python,
            &token,
        )
        .await
        {
            Ok(()) => {
                return Ok(Python {
                    path: previous.python_path.clone(),
                    install: previous.python_install.clone(),
                });
            }
            Err(err) => tracing::info!(cause = %err, "the active Python is not reused"),
        }
    }
    install_python(step, env, &toolchain.python, force, leftovers).await
}

/// The new release's file manifest written (spec, decision 25), on the
/// blocking pool, right after uv installed it from an archive whose
/// checksum uv verified. A failure only goes to the run log: the release
/// is then "not checked" by `--verify-venv` until its next install.
async fn manifest_release(layout: &Layout, release: &Path) {
    let stored = crate::verify::manifest_path(layout, release);
    let release = release.to_path_buf();
    let written = sb_cli_core::spawn::blocking(move || {
        let owner = sb_cli_core::root::effective_uid();
        crate::verify::write_release_manifest(&release, &stored, owner)
    })
    .await;
    match written {
        Ok(Ok(files)) => tracing::info!(files, "Python release manifest written"),
        Ok(Err(err)) => tracing::warn!(%err, "no Python release manifest"),
        Err(err) => tracing::warn!(%err, "no Python release manifest"),
    }
}

/// A new Python release for `version` (sb-go's `createPythonRelease`):
/// its directory, "Installing Python <v>", uv's find and the Python
/// check.
async fn install_python<'a>(
    step: &mut Step,
    env: &Env<'a>,
    version: &String,
    force: Force,
    leftovers: &mut Leftovers<'a>,
) -> Result<Python, BuildError> {
    let token = step.token();
    let id = format!(
        "python-{}-{}",
        version.replace('.', "-"),
        stamp(jiff::Timestamp::now())
    );
    let dir = env.layout.python_releases.join(id);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&dir)
        .map_err(BuildError::PythonDir)?;
    leftovers.python = Some(dir.clone());
    let dir_arg = dir.to_string_lossy().into_owned();
    let installing = format!("Installing Python {version}");
    nested(step, Labels::new(&installing, &installing), async |step| {
        let install = env
            .isolated(Kind::Uv { cache: true }, &env.layout.uv)?
            .map(|cmd| {
                let cmd = cmd
                    .arg("python")
                    .arg("install")
                    .arg("--managed-python")
                    .arg("--no-bin")
                    .arg("--install-dir")
                    .shown_value(dir_arg.clone());
                let cmd = if force == Force::Python {
                    cmd.arg("--reinstall")
                } else {
                    cmd
                };
                let cmd = if force == Force::None {
                    cmd
                } else {
                    cmd.arg("--no-cache")
                };
                cmd.shown_value(version.clone())
            });
        let _streamed = install
            .cmd
            .stream(step)
            .await?
            .check()
            .with_context(|| format!("error installing Python {version}"))
            .context("install Python release")?;
        manifest_release(env.layout, &dir).await;
        anyhow::Ok(())
    })
    .await?;
    let find = env
        .isolated(Kind::Uv { cache: true }, &env.layout.uv)?
        .map(|cmd| {
            cmd.arg("python")
                .arg("find")
                .arg("--managed-python")
                .arg("--no-project")
                .arg("--no-python-downloads")
                .shown_value(version.clone())
                .env("UV_PYTHON_INSTALL_DIR", dir_arg.clone())
                .timeout(FIND_TIMEOUT)
        });
    let found = crate::captured(&find.cmd, &token)
        .await
        .map_err(|cause| BuildError::Find {
            version: version.clone(),
            cause,
        })?;
    let found = found.trim();
    if !Path::new(found).is_absolute() {
        return Err(BuildError::NotAbsolute {
            path: found.to_string(),
            version: version.clone(),
        });
    }
    check_python(env, Path::new(found), version, &token)
        .await
        .map_err(BuildError::Python)?;
    // Go's `filepath.Clean` for an absolute path uv printed: repeated
    // and trailing separators and `.` parts go (std's `components`,
    // <https://doc.rust-lang.org/std/path/struct.Path.html#method.components>;
    // `..` stays, which uv never prints).
    let found: PathBuf = Path::new(found).components().collect();
    let found = found.to_string_lossy().into_owned();
    Ok(Python {
        path: found,
        install: dir_arg,
    })
}

/// Build a new generation into staging (spec, steps 7-11): uv's cache
/// cleared when forced ([`clear_cache`]); the Python
/// (the active one's, or a new release); `releases/<stamp>-<lock[:12]>/`; "Creating Ansible
/// virtual environment" (`uv venv`); the requirements ([`requirements`]:
/// hashed, or unhashed for a pin-less checkout), both streamed into their
/// steps with no timeout (Ctrl-C stops them); the exported commands; "Validating the new Ansible virtual
/// environment" ([`validate`]); the manifest. `previous` is the active generation's manifest, if any.
///
/// # Errors
///
/// [`BuildError`]: the first part that fails; everything the build made
/// is removed by then.
pub async fn build<'a>(
    step: &mut Step,
    env: &Env<'a>,
    toolchain: &Toolchain,
    previous: Option<&Manifest>,
    force: Force,
) -> Result<Staged<'a>, BuildError> {
    let mut leftovers = Leftovers {
        layout: env.layout,
        generation: None,
        python: None,
        armed: true,
    };
    match stage(step, env, toolchain, previous, force, &mut leftovers).await {
        Ok((generation, venv, manifest)) => Ok(Staged {
            leftovers,
            generation,
            venv,
            manifest,
        }),
        Err(err) => {
            leftovers.remove().await;
            Err(err)
        }
    }
}

/// [`build`]'s parts, each thing made recorded in `leftovers`.
async fn stage<'a>(
    step: &mut Step,
    env: &Env<'a>,
    toolchain: &Toolchain,
    previous: Option<&Manifest>,
    force: Force,
    leftovers: &mut Leftovers<'a>,
) -> Result<(PathBuf, PathBuf, Manifest), BuildError> {
    if force != Force::None {
        clear_cache(step, env).await?;
    }
    let Python {
        path: python_path,
        install: python_install,
    } = python(step, env, toolchain, previous, force, leftovers).await?;
    let id = format!(
        "{}-{}",
        stamp(jiff::Timestamp::now()),
        &toolchain.lock_sha256[..12]
    );
    let generation = env.layout.ansible_releases.join(&id);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&generation)
        .map_err(|cause| BuildError::Generation {
            id: id.clone(),
            cause,
        })?;
    leftovers.generation = Some(generation.clone());
    let venv = generation.join("venv");
    let creating = "Creating Ansible virtual environment";
    nested(step, Labels::new(creating, creating), async |step| {
        let create = env
            .isolated(Kind::Uv { cache: true }, &env.layout.uv)?
            .map(|cmd| {
                cmd.arg("venv")
                    .arg("--python")
                    .shown_value(python_path.clone())
                    .arg("--no-project")
                    .arg("--no-python-downloads")
                    .shown_value(venv.to_string_lossy())
            });
        let _streamed = create
            .cmd
            .stream(step)
            .await?
            .check()
            .context("error creating venv")?;
        anyhow::Ok(())
    })
    .await?;
    requirements(step, env, toolchain, &venv, force).await?;
    let commands = discover(&venv)?;
    validate(step, env, &venv, &toolchain.python).await?;
    let manifest = Manifest {
        schema_version: manifest::SCHEMA,
        generation_id: id,
        python_version: toolchain.python.clone(),
        python_path,
        python_install,
        uv_version: env.uv_version.to_string(),
        lock_sha256: toolchain.lock_sha256.clone(),
        saltbox_commit: toolchain.commit.clone(),
        created_at: created_at(jiff::Timestamp::now()),
        exported_commands: commands,
        requirements_unhashed: !toolchain.hashed,
    };
    manifest::write(&generation, &manifest)?;
    Ok((generation, venv, manifest))
}

/// The requirements into the staged venv (spec, step 9): "Syncing hashed
/// Saltbox requirements" (`uv pip sync --require-hashes`) for a pinned
/// checkout; without a pin, "Installing unhashed Saltbox requirements"
/// (`uv pip install --requirement`, which resolves the list's
/// dependencies, where `pip sync` installs only what is listed: sb-go
/// 69b968b's `installRequirementsArgs`). Either streamed into its step
/// with no timeout, `--no-cache` when forced.
async fn requirements(
    step: &mut Step,
    env: &Env<'_>,
    toolchain: &Toolchain,
    venv: &Path,
    force: Force,
) -> Result<(), BuildError> {
    let (label, failure) = if toolchain.hashed {
        (
            "Syncing hashed Saltbox requirements",
            "error syncing requirements",
        )
    } else {
        (
            "Installing unhashed Saltbox requirements",
            "error installing unhashed requirements",
        )
    };
    nested(step, Labels::new(label, label), async |step| {
        let python = venv.join("bin/python3");
        let lock = env.layout.lock.to_string_lossy();
        let install = env
            .isolated(Kind::Uv { cache: true }, &env.layout.uv)?
            .map(|cmd| {
                let cmd = cmd
                    .arg("pip")
                    .arg(if toolchain.hashed { "sync" } else { "install" })
                    .arg("--no-progress")
                    .arg("--python")
                    .shown_value(python.to_string_lossy());
                let cmd = if toolchain.hashed {
                    cmd.arg("--require-hashes")
                } else {
                    cmd
                };
                let cmd = if force == Force::None {
                    cmd
                } else {
                    cmd.arg("--no-cache")
                };
                if toolchain.hashed {
                    cmd.shown_value(lock)
                } else {
                    cmd.arg("--requirement").shown_value(lock)
                }
            });
        let _streamed = install.cmd.stream(step).await?.check().context(failure)?;
        anyhow::Ok(())
    })
    .await
}

/// "Validating the new Ansible virtual environment" (spec, step 11;
/// decision 24): the environment check on the staged venv, whose probes
/// take about two seconds on a Saltbox venv, in a step of its own.
async fn validate(
    step: &mut Step,
    env: &Env<'_>,
    venv: &Path,
    version: &str,
) -> Result<(), BuildError> {
    let validating = Labels::new(
        "Validating the new Ansible virtual environment",
        "New Ansible virtual environment validated",
    );
    nested(step, validating, async |step| {
        check_environment(env, venv, version, &step.token())
            .await
            .map_err(BuildError::Staged)?;
        anyhow::Ok(())
    })
    .await
}

/// "Clearing the uv cache" (spec, step 7; difference 18) before a forced
/// build, which the retry is too: `uv cache clean`, which "removes all
/// cache entries from the cache directory", sb's own (`UV_CACHE_DIR`)
/// (<https://docs.astral.sh/uv/concepts/cache/#clearing-the-cache>; the
/// same page: "it's never safe to modify the cache directly"). uv
/// clones a venv's files from their cache entries, or hardlinks them
/// where the filesystem can't clone (ext4), its `link-mode` default on
/// Linux (<https://docs.astral.sh/uv/reference/settings/#link-mode>); a
/// hardlinked venv file damaged in place damages its entry too, and the
/// next normal build would bring it back. Streamed into the step with no
/// timeout: uv waits at most 5 minutes for another uv process's lock
/// (that page, "Clearing the cache"; not `--force`, which ignores the
/// lock), and Ctrl-C stops it.
async fn clear_cache(step: &mut Step, env: &Env<'_>) -> Result<(), BuildError> {
    let clearing = Labels::new("Clearing the uv cache", "uv cache cleared");
    nested(step, clearing, async |step| {
        let clean = env
            .isolated(Kind::Uv { cache: true }, &env.layout.uv)?
            .map(|cmd| cmd.arg("cache").arg("clean"));
        let _streamed = clean
            .cmd
            .stream(step)
            .await?
            .check()
            .context("clear uv cache")?;
        anyhow::Ok(())
    })
    .await
}

/// A UTC time in Go's `20060102T150405.000000000` (generation and
/// Python release names).
pub(crate) fn stamp(at: jiff::Timestamp) -> String {
    at.strftime(STAMP).to_string()
}

/// A UTC time in Go's `time.RFC3339Nano` (the manifest's `created_at`):
/// jiff's RFC 3339 `Display`, `Z` and the fraction's trailing zeros cut,
/// none when it is zero
/// (<https://docs.rs/jiff/0.2.38/jiff/struct.Timestamp.html#impl-Display-for-Timestamp>).
fn created_at(at: jiff::Timestamp) -> String {
    at.to_string()
}

/// The commands a staged venv exports (sb-go's `discoverCommands`; spec,
/// step 10): regular executable files in `bin` named `ansible*`, `certbot`
/// or `apprise`, sorted; `ansible`, `certbot` and `apprise` required. A
/// symlink is no command: sb-go reads `lstat`'s type, and std's
/// `DirEntry::metadata` doesn't follow symlinks
/// (<https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata>).
fn discover(venv: &Path) -> Result<Vec<String>, BuildError> {
    let bin = venv.join("bin");
    let entries = std::fs::read_dir(&bin).map_err(BuildError::Commands)?;
    let mut commands = Vec::new();
    for entry in entries {
        let entry = entry.map_err(BuildError::Commands)?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("ansible") || name == "certbot" || name == "apprise") {
            continue;
        }
        let meta = entry.metadata().map_err(|cause| BuildError::Command {
            name: name.to_string(),
            cause,
        })?;
        if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
            if name.contains(['\n', '\r']) || entry.file_name().to_str().is_none() {
                return Err(BuildError::UnsafeName(name.to_string()));
            }
            commands.push(name.into_owned());
        }
    }
    commands.sort();
    for required in ENTRY_POINTS {
        if !commands.iter().any(|name| name == required) {
            return Err(BuildError::Missing(required));
        }
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: i64, nanosecond: i32) -> jiff::Timestamp {
        jiff::Timestamp::new(second, nanosecond).expect("timestamp")
    }

    /// 2026-08-07T12:00:00Z, sb-go's test generation's time.
    const NOON: i64 = 1_786_104_000;

    /// Go's `20060102T150405.000000000`: always nine digits.
    #[test]
    fn stamps_are_gos_layout() {
        assert_eq!(stamp(at(NOON, 0)), "20260807T120000.000000000");
        assert_eq!(stamp(at(NOON, 1_200)), "20260807T120000.000001200");
    }

    /// Go's `RFC3339Nano`: trailing zeros cut, no fraction when zero.
    #[test]
    fn created_at_is_rfc3339_nano() {
        assert_eq!(created_at(at(NOON, 0)), "2026-08-07T12:00:00Z");
        assert_eq!(created_at(at(NOON, 120_000_000)), "2026-08-07T12:00:00.12Z");
        assert_eq!(
            created_at(at(NOON, 123_456_789)),
            "2026-08-07T12:00:00.123456789Z"
        );
    }

    fn executable(path: &Path) {
        sb_cli_core::testing::write_program(path, "#!/bin/sh\n");
    }

    /// sb-go's `TestDiscoverCommands`: regular executables named
    /// `ansible*`, `certbot` or `apprise`, sorted; a symlink, a plain file,
    /// a directory and other names left out.
    #[test]
    fn commands_are_the_venvs_entry_points() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        for name in [
            "certbot",
            "ansible-playbook",
            "apprise",
            "ansible",
            "pip",
            "python3",
        ] {
            executable(&bin.join(name));
        }
        std::fs::write(bin.join("ansible-notes"), "").expect("plain file");
        std::fs::create_dir(bin.join("ansible-dir")).expect("dir");
        std::os::unix::fs::symlink(bin.join("ansible"), bin.join("ansible-link")).expect("link");
        assert_eq!(
            discover(dir.path()).expect("commands"),
            ["ansible", "ansible-playbook", "apprise", "certbot"]
        );
    }

    /// sb-go's `TestDiscoverCommandsRequiresEntrypoints`.
    #[test]
    fn each_entry_point_is_required() {
        for missing in ENTRY_POINTS {
            let dir = tempfile::tempdir().expect("tempdir");
            let bin = dir.path().join("bin");
            std::fs::create_dir(&bin).expect("bin");
            for name in ENTRY_POINTS.iter().filter(|name| **name != missing) {
                executable(&bin.join(name));
            }
            let err = discover(dir.path()).expect_err(missing);
            assert_eq!(
                err.to_string(),
                format!("required command {missing} is missing from staged venv")
            );
        }
    }

    #[test]
    fn a_command_name_with_a_line_break_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        for name in ENTRY_POINTS {
            executable(&bin.join(name));
        }
        executable(&bin.join("ansible\nx"));
        let err = discover(dir.path()).expect_err("unsafe");
        assert_eq!(err.to_string(), "unsafe venv command name \"ansible\\nx\"");
    }

    /// A tree with an active `old` generation and a staged `new` one
    /// exporting `ansible`.
    fn staged_tree(layout: &Layout) -> (PathBuf, Staged<'_>) {
        let old = layout.ansible_releases.join("old/venv");
        let generation = layout.ansible_releases.join("new");
        std::fs::create_dir_all(&old).expect("old");
        std::fs::create_dir_all(generation.join("venv")).expect("new");
        std::fs::create_dir_all(&layout.wrappers).expect("bin");
        std::os::unix::fs::symlink(&old, &layout.active_venv).expect("link");
        let staged = Staged {
            leftovers: Leftovers {
                layout,
                generation: Some(generation.clone()),
                python: None,
                armed: true,
            },
            venv: generation.join("venv"),
            generation: generation.clone(),
            manifest: Manifest {
                schema_version: manifest::SCHEMA,
                generation_id: "new".to_string(),
                python_version: "3.12.7".to_string(),
                python_path: String::new(),
                python_install: String::new(),
                uv_version: "0.13.4".to_string(),
                lock_sha256: String::new(),
                saltbox_commit: String::new(),
                created_at: String::new(),
                exported_commands: vec!["ansible".to_string()],
                requirements_unhashed: false,
            },
        };
        (old, staged)
    }

    /// Ctrl-C after the build, before activation: the build goes, the
    /// active link and the wrappers stay (spec, step 17).
    #[tokio::test]
    async fn a_cancel_before_activation_keeps_the_active_venv() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = Layout::under(dir.path());
        let (old, staged) = staged_tree(&layout);
        let generation = staged.generation.clone();
        let fake = sb_release::testing::Fake::start().expect("fake");
        let releases = fake.releases("sb/test").expect("client");
        let env = Env::new(&layout, &releases, "0.13.4", sb_release::Arch::Amd64);
        let token = CancellationToken::new();
        token.cancel();
        let err = activate_staged(&env, staged, &token)
            .await
            .expect_err("cancelled");
        assert!(matches!(err, VenvError::Interrupted), "{err}");
        assert_eq!(std::fs::read_link(&layout.active_venv).expect("link"), old);
        assert!(!generation.exists());
        assert!(
            !layout.wrappers.join("ansible").exists(),
            "no wrapper written"
        );
    }

    /// Wrappers that can't be written stop the activation: the build goes,
    /// the active link stays.
    #[tokio::test]
    async fn a_failed_wrapper_keeps_the_active_venv() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = Layout::under(dir.path());
        let (old, staged) = staged_tree(&layout);
        let generation = staged.generation.clone();
        std::fs::remove_dir(&layout.wrappers).expect("rm bin");
        let fake = sb_release::testing::Fake::start().expect("fake");
        let releases = fake.releases("sb/test").expect("client");
        let env = Env::new(&layout, &releases, "0.13.4", sb_release::Arch::Amd64);
        let err = activate_staged(&env, staged, &CancellationToken::new())
            .await
            .expect_err("no bin");
        assert!(
            err.to_string()
                .starts_with("install Ansible command wrappers: "),
            "{err}"
        );
        assert_eq!(std::fs::read_link(&layout.active_venv).expect("link"), old);
        assert!(!generation.exists());
    }

    /// The retry rule (spec, Testing Strategy): the normal path only,
    /// never after a cancel.
    #[test]
    fn only_the_normal_path_retries_and_never_after_a_cancel() {
        assert!(retries(Force::None, false));
        assert!(!retries(Force::None, true));
        for force in [Force::Venv, Force::Python] {
            assert!(!retries(force, false), "{force:?}");
            assert!(!retries(force, true), "{force:?}");
        }
    }

    /// The retry's cause: a nested step's own error, else the build's.
    #[test]
    fn a_builds_cause_is_its_steps_error() {
        let stopped = BuildError::Stopped {
            exit: Exit(1),
            cause: Some("error syncing requirements: exit 1".to_string()),
        };
        assert_eq!(stopped.cause(), "error syncing requirements: exit 1");
        let quiet = BuildError::Stopped {
            exit: Exit(1),
            cause: None,
        };
        assert_eq!(
            quiet.cause(),
            "a step stopped the Ansible virtual environment's build"
        );
        assert_eq!(
            BuildError::Missing("certbot").cause(),
            "required command certbot is missing from staged venv"
        );
    }

    #[test]
    fn a_venv_without_bin_is_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = discover(dir.path()).expect_err("no bin");
        assert!(err.to_string().starts_with("read venv commands: "), "{err}");
    }
}
