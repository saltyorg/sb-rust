//! `sb update`: sb-ops' update (ops spec, "`sb update`") on the host's
//! apt and disks, the Saltbox checkout at the Saltbox root
//! (`SB_SALTBOX_DIR`, else `/srv/git/saltbox`), the Sandbox checkout under
//! the inventory's appdata path, and sb's own file for self-update.

use std::path::PathBuf;

use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;
use sb_host::apt::Apt;
use sb_host::disk::Disk;
use sb_ops::branch::Checkout;
use sb_ops::update::{RERUN, Update, UpdateArgs};
use sb_release::{Releases, SelfUpdate};
use sb_term_ui::{Options, Runner};

use crate::self_update::{SB_REPO, executable};
use crate::setup::ask_branch;

/// `sb update`.
///
/// # Errors
///
/// sb-ops' (a refusal before any step, self-update's failure, an
/// unanswered question, a failed step), the release client or sb's own
/// path (before anything runs), or the runtime not built.
pub fn update(args: &UpdateArgs, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let appdata = config::appdata_path(&config::inventory_path(&root));
    let sandbox = Checkout::Sandbox.path(&root);
    let build = BuildInfo::current(env!("CARGO_PKG_VERSION"));
    let releases = Releases::for_sb(&format!("sb/{}", build.version))?;
    // sb's own file only when self-update runs: a replaced sb is refused
    // there (`executable`), not in an update that skips it.
    let target = if args.skip_self_update {
        None
    } else {
        Some(executable(std::env::current_exe())?)
    };
    let self_update = target.as_deref().map(|target| SelfUpdate {
        repo: SB_REPO,
        build: &build,
        target,
        yes: true,
        force: false,
        message: Some(RERUN),
        shutdown,
    });
    // sb-go's three (cmd/update.go): the root, the appdata path, /srv.
    let disk_paths = [PathBuf::from("/"), appdata, PathBuf::from("/srv")];
    let (disk, apt) = (Disk::new(), Apt::new());
    let update = Update {
        saltbox: &root,
        sandbox: &sandbox,
        disk: &disk,
        disk_paths: &disk_paths,
        apt: &apt,
        releases: &releases,
        self_update,
        args,
    };
    let ask = |shown: &str, question: &str| ask_branch(shown, question, shutdown);
    let runner = || Runner::start("sb update", shutdown, log, Options::default());
    super::block_on(sb_ops::update::run(
        &update,
        sb_term_ui::can_ask(),
        ask,
        runner,
        shutdown,
    ))?
}
