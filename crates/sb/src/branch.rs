//! `sb branch <name>` and `sb branch-sandbox <name>`: sb-ops' branch
//! commands (ops spec), run on sb's runtime under the Saltbox directory
//! (`SB_SALTBOX_DIR`, else `/srv/git/saltbox`), with python-env's layout
//! for Saltbox's runtime and `ansible-playbook` for the tags.

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;
use sb_ops::Checkout;
use sb_ops::branch::Tools;

/// `checkout` switched to the branch `name` (sb-ops' `branch::run`).
///
/// # Errors
///
/// sb-ops' (the checkout, the name or the Saltbox user, before anything
/// changes; the switch's failed step), the release client or the
/// architecture, or the runtime not built.
pub fn branch(
    checkout: Checkout,
    name: &str,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let ansible = super::ansible::ansible();
    super::python_env::with_env(log, |python, _| {
        let tools = Tools {
            python,
            ansible: &ansible,
        };
        super::block_on(sb_ops::branch::run(
            checkout, &root, name, tools, shutdown, log,
        ))?
    })
}
