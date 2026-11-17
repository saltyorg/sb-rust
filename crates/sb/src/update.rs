//! `sb update`: sb-ops' update (ops spec, "`sb update`") on the host's
//! apt and disks, the Saltbox checkout at the Saltbox root
//! (`SB_SALTBOX_DIR`, else `/srv/git/saltbox`), the Sandbox checkout under
//! the inventory's appdata path, python-env's layout for Saltbox's
//! runtime, `ansible-playbook` for the tags, validate-config's settings,
//! and sb's own file for self-update.

use std::io::IsTerminal as _;
use std::path::PathBuf;

use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;
use sb_host::apt::Apt;
use sb_host::disk::Disk;
use sb_ops::completions::Refresh;
use sb_ops::update::{RERUN, Update, UpdateArgs};
use sb_release::SelfUpdate;
use sb_term_ui::{Options, Runner};

use crate::self_update::{SB_REPO, executable};
use crate::setup::ask_branch;

/// `sb update`.
///
/// # Errors
///
/// sb-ops' (a refusal before any step, self-update's failure, an
/// unanswered question, a failed step), the release client, the HTTP
/// client or sb's own path (before anything runs), or the runtime not
/// built.
pub fn update(args: &UpdateArgs, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let appdata = config::appdata_path(&config::inventory_path(&root));
    let build = BuildInfo::current(env!("CARGO_PKG_VERSION"));
    let settings = super::validate_settings()?;
    let backups = super::backups_dir();
    let ansible = super::ansible::ansible();
    // sb's own file only when self-update runs: a replaced sb is refused
    // there (`executable`), not in an update that skips it.
    let target = if args.skip_self_update {
        None
    } else {
        Some(executable(std::env::current_exe())?)
    };
    // sb-go's three (cmd/update.go): the root, the appdata path, /srv.
    let disk_paths = [PathBuf::from("/"), appdata, PathBuf::from("/srv")];
    let (disk, apt) = (Disk::new(), Apt::new());
    // The completion refresh's shells' directories (a test's root under
    // the `testing` feature) and sb's own executable (step 12, M87).
    let (completion_paths, executable) = (
        sb_completion::install::Paths::for_sb(),
        std::env::current_exe(),
    );
    // A second signal SIGKILLs the children still running (a migration's
    // playbook, apt, git), as `sb install` registers it (exec decision 12).
    let _children = sb_exec::on_forced_exit(shutdown);
    super::python_env::with_env(log, |python, releases| {
        let self_update = target.as_deref().map(|target| SelfUpdate {
            repo: SB_REPO,
            build: &build,
            target,
            yes: true,
            force: false,
            message: Some(RERUN),
            shutdown,
        });
        let update = Update {
            root: &root,
            disk: &disk,
            disk_paths: &disk_paths,
            apt: &apt,
            releases,
            self_update,
            args,
            python,
            ansible: &ansible,
            settings: &settings,
            backups: &backups,
            // The new announcements in tui's viewer on a terminal (M87).
            stdout_terminal: std::io::stdout().is_terminal(),
            completions: Refresh {
                paths: &completion_paths,
                executable: &executable,
            },
        };
        let ask = |shown: &str, question: &str| ask_branch(shown, question, shutdown);
        // The migrations' question on the terminal, `--migrations` its flag.
        let confirm =
            |question: &str| sb_term_ui::confirm(question, None, "--migrations", shutdown);
        let runner = || Runner::start("sb update", shutdown, log, Options::default());
        super::block_on(sb_ops::update::run(
            &update,
            sb_term_ui::can_ask(),
            ask,
            confirm,
            runner,
            shutdown,
            &mut anstream::stderr(),
        ))?
    })
}
