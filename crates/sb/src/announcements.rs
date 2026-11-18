//! `sb announcements` (ops spec, "`sb announcements`"): Saltbox's and
//! Sandbox's announcements, read on sb's current-thread runtime; its
//! hidden `preview` and `check` (decisions 34 and 35).

use std::io::IsTerminal as _;
use std::path::Path;

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;

/// `sb announcements`. The runtime is built here, after parsing, as for
/// the other commands; the Saltbox root is `SB_SALTBOX_DIR`'s (ops
/// assumption 5).
///
/// # Errors
///
/// sb-ops' command's: a failed write, the viewer's.
pub fn announcements(shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    super::block_on(sb_ops::announcements::announcements(&root, shutdown, log))?
}

/// `sb announcements preview`: sb-ops' preview at `since`, `repo` alone if
/// given, the migrations run with `run_migrations`; `update`'s question
/// on the terminal, its runner and `ansible-playbook`.
///
/// # Errors
///
/// sb-ops' preview's.
pub fn preview(
    since: &str,
    repo: Option<sb_ops::Checkout>,
    run_migrations: bool,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let ansible = super::ansible::ansible();
    let preview = sb_ops::announcements::preview::Preview {
        root: &root,
        since,
        repo,
        run_migrations,
        ansible: &ansible,
        stdout_terminal: std::io::stdout().is_terminal(),
    };
    // A second signal SIGKILLs a migration's playbook still running, as
    // `sb update` registers it (exec decision 12).
    let _children = sb_exec::on_forced_exit(shutdown);
    let confirm =
        |question: &str| sb_term_ui::confirm(question, None, "--run-migrations", shutdown);
    let runner = || {
        sb_term_ui::Runner::start(
            "sb announcements preview",
            shutdown,
            log,
            sb_term_ui::Options::default(),
        )
    };
    super::block_on(sb_ops::announcements::preview::preview(
        &preview,
        sb_term_ui::can_ask(),
        confirm,
        runner,
        shutdown,
        &mut anstream::stderr(),
    ))?
}

/// `sb announcements check`: sb-ops' check of `file`, or both checkouts'
/// own files, its lines on stdout.
///
/// # Errors
///
/// sb-ops' check's: its problems counted, no file, a failed write.
pub fn check(
    file: Option<&Path>,
    repo: Option<sb_ops::Checkout>,
    shutdown: &Shutdown,
) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let check = sb_ops::announcements::check::Check {
        root: &root,
        file,
        repo,
    };
    super::block_on(sb_ops::announcements::check::check(
        &check,
        shutdown,
        &mut std::io::stdout().lock(),
    ))?
}
