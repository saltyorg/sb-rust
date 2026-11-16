//! `sb gha`, hidden: sb-ops' gha (ops spec, "`sb gha` (hidden)") on the
//! host's apt, `/srv`, locale and Saltbox root (`SB_SALTBOX_DIR`, else
//! `/srv/git/saltbox`), python-env's layout, and `SUDO_USER` as the
//! configs' owner. Saltbox's CI runs it as its `runner` user, so sb has
//! relaunched itself through sudo and `SUDO_USER` names that user.

use sb_cli_core::build_info::{BuildInfo, UV_VERSION};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;
use sb_host::apt::Apt;
use sb_ops::gha::Gha;
use sb_ops::locale::Locale;
use sb_ops::prereq::Dirs;
use sb_python_env::{Env, Layout};
use sb_release::{Arch, Releases};
use sb_term_ui::{Options, Runner};

/// `sb gha`.
///
/// # Errors
///
/// The release client or the architecture (before anything runs), the
/// runtime not built, or the run's failed step.
pub fn gha(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let layout = Layout::for_sb();
    let build = BuildInfo::current(env!("CARGO_PKG_VERSION"));
    // uv's and the fact's releases through the proxy or GitHub, as python-env's
    // commands reach them; in a test build, the fake GitHub only.
    let releases = Releases::for_sb(&format!("sb/{}", build.version))?;
    let mut python = Env::for_sb(&layout, &releases, UV_VERSION, Arch::current()?);
    // The apt sources' backup is named by the run log's id; without a log,
    // python-env's own.
    if let Some(log) = log {
        log.id().clone_into(&mut python.run_id);
    }
    let sudo_user = std::env::var("SUDO_USER").ok();
    let (apt, dirs, locale) = (Apt::new(), Dirs::saltbox(), Locale::system());
    let gha = Gha {
        apt: &apt,
        dirs: &dirs,
        locale: &locale,
        bash: std::path::Path::new("bash"),
        root: &root,
        python: &python,
        run_id: &python.run_id,
        sudo_user: sudo_user.as_deref(),
    };
    super::block_on(async {
        let mut runner = Runner::start("sb gha", shutdown, log, Options::default());
        let result = sb_ops::gha::run(&mut runner, &gha).await;
        runner.finish();
        result.map_err(anyhow::Error::from)
    })?
}
