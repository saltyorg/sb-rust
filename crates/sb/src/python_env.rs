//! python-env's commands (python-env spec, "The commands"): the hidden
//! `sb update-venv` (module build M14), `sb reinstall-venv` and `sb
//! reinstall-python` (M16).

use anyhow::Context as _;
use sb_cli_core::build_info::{BuildInfo, UV_VERSION};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;
use sb_python_env::{Env, Force, Layout, reconcile_runtime, reconcile_venv};
use sb_release::{Arch, Releases};
use sb_term_ui::{Labels, Options, Runner, Step};

/// The Saltbox user (config's `saltbox_user`, under `SB_SALTBOX_DIR`'s
/// root as sb finds its other files), with sb-go's wrapping (python-env
/// spec, "Amendments to other specs": config), on one line as sb-go
/// prints it. `SB_SALTBOX_DIR` (tests and debugging; sudo's relaunch
/// drops it) moves only where accounts.yml is read: the venv is
/// reconciled against python-env's fixed layout, which only the test
/// build's `SB_TEST_PYTHON_ENV_ROOT` moves.
fn saltbox_user() -> anyhow::Result<String> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    sb_config::user::saltbox_user(&root)
        .map_err(|err| anyhow::anyhow!("error getting saltbox user: {err}"))
}

/// The Saltbox user read first, then `body` in one outer step of a
/// runner named `name`, on python-env's layout.
fn run(
    name: &str,
    labels: Labels,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
    body: impl AsyncFnOnce(&mut Step, &Env<'_>, &str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let user = saltbox_user()?;
    let layout = Layout::for_sb();
    let build = BuildInfo::current(env!("CARGO_PKG_VERSION"));
    // uv's release (and the fact's) through the proxy or GitHub, as `sb
    // self-update` reaches sb's; in a test build, the fake GitHub only.
    let releases = Releases::for_sb(&format!("sb/{}", build.version))?;
    let env = Env::for_sb(&layout, &releases, UV_VERSION, Arch::current()?);
    super::block_on(async {
        let mut runner = Runner::start(name, shutdown, log, Options::default());
        let result = runner
            .step(labels, async |step| body(step, &env, &user).await)
            .await;
        runner.finish();
        result.map_err(anyhow::Error::from)
    })?
}

/// `sb update-venv`: `reconcile_runtime` under sb-go's labels
/// (cmd/updateVenv.go).
///
/// # Errors
///
/// The user's error before anything runs, or the step's failure.
pub fn update_venv(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let labels = Labels::new(
        "Checking Ansible virtual environment and saltbox.fact for updates",
        "Ansible virtual environment and saltbox.fact are ready",
    )
    .failure("Saltbox runtime dependency update check");
    run(
        "sb update-venv",
        labels,
        shutdown,
        log,
        async |step, env, user| {
            reconcile_runtime(step, env, Some(user)).await?;
            Ok(())
        },
    )
}

/// `sb reinstall-venv`: a new venv on the active Python
/// (`reconcile_venv` with `Force::Venv`) under sb-go's labels and error
/// (cmd/reinstallVenv.go).
///
/// # Errors
///
/// The user's error before anything runs, or the step's failure.
pub fn reinstall_venv(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let labels = Labels::new(
        "Reinstalling Ansible virtual environment",
        "Ansible virtual environment reinstalled",
    );
    run(
        "sb reinstall-venv",
        labels,
        shutdown,
        log,
        async |step, env, user| {
            reconcile_venv(step, env, Some(user), Force::Venv)
                .await
                .context("error managing Ansible venv")?;
            Ok(())
        },
    )
}

/// `sb reinstall-python`: a new Python release and venv
/// (`reconcile_venv` with `Force::Python`) in sb-go's two steps
/// (cmd/reinstallPython.go). sb-go reads the user inside the outer step;
/// sb reads it first, as the spec's table has it for every command.
///
/// # Errors
///
/// The user's error before anything runs, or the step's failure.
pub fn reinstall_python(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let labels = Labels::new(
        "Reinstalling Saltbox Python and Ansible environment",
        "Saltbox Python and Ansible environment reinstalled",
    )
    .failure("Python and Ansible environment reinstall");
    run(
        "sb reinstall-python",
        labels,
        shutdown,
        log,
        async |step, env, user| {
            let recreating = Labels::new(
                "Recreating Ansible virtual environment",
                "Ansible virtual environment recreated",
            )
            .failure("Ansible virtual environment");
            step.step(recreating, async |step| {
                reconcile_venv(step, env, Some(user), Force::Python)
                    .await
                    .context("error managing Ansible venv")?;
                anyhow::Ok(())
            })
            .await?;
            Ok(())
        },
    )
}
