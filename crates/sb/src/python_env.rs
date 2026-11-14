//! python-env's commands (python-env spec, "The commands"): so far the
//! hidden `sb update-venv` (module build M14).

use sb_cli_core::build_info::UV_VERSION;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;
use sb_python_env::{Env, Layout, reconcile_runtime};
use sb_term_ui::{Labels, Options, Runner};

/// `sb update-venv`: the Saltbox user read first (config's
/// `saltbox_user`, under `SB_SALTBOX_DIR`'s root as sb finds its other
/// files), then `reconcile_runtime` under sb-go's labels
/// (cmd/updateVenv.go). `SB_SALTBOX_DIR` (tests and debugging; sudo's
/// relaunch drops it) moves only where accounts.yml is read: the venv is
/// reconciled against python-env's fixed layout, which only the test
/// build's `SB_TEST_PYTHON_ENV_ROOT` moves.
///
/// # Errors
///
/// The user's error before anything runs, or the step's failure.
pub fn update_venv(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    // sb-go's wrapping (python-env spec, "Amendments to other specs":
    // config), on one line as sb-go prints it.
    let user = sb_config::user::saltbox_user(&root)
        .map_err(|err| anyhow::anyhow!("error getting saltbox user: {err}"))?;
    let layout = Layout::for_sb();
    let env = Env::for_sb(&layout, UV_VERSION);
    super::block_on(async {
        let mut runner = Runner::start("sb update-venv", shutdown, log, Options::default());
        let labels = Labels::new(
            "Checking Ansible virtual environment and saltbox.fact for updates",
            "Ansible virtual environment and saltbox.fact are ready",
        )
        .failure("Saltbox runtime dependency update check");
        let result = runner
            .step(labels, async |step| {
                reconcile_runtime(step, &env, Some(&user)).await?;
                Ok(())
            })
            .await;
        runner.finish();
        result.map_err(anyhow::Error::from)
    })?
}
