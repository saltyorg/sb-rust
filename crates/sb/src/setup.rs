//! `sb setup`, hidden: sb-ops' setup (ops spec, "`sb setup` (hidden)") on
//! the host's checks, apt, `/srv`, locale and Saltbox root
//! (`SB_SALTBOX_DIR`, else `/srv/git/saltbox`), Saltbox's repository and
//! python-env's layout. The guard and the branch question come before
//! the runner starts, so a refusal or an unanswerable question changes
//! nothing.

use std::io::Write as _;

use sb_cli_core::build_info::{BuildInfo, UV_VERSION};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::exit;
use sb_cli_core::log::RunLog;
use sb_host::apt::Apt;
use sb_ops::completions::Refresh;
use sb_ops::locale::Locale;
use sb_ops::prereq::Dirs;
use sb_ops::setup::{BRANCH_FLAGS, SALTBOX_URL, Setup, SetupArgs};
use sb_python_env::{Env, Layout};
use sb_release::{Arch, Releases};
use sb_term_ui::{Options, Runner};

/// `sb setup`.
///
/// # Errors
///
/// The guard's refusal, the branch question unanswered, the release
/// client or the architecture (all before any step), the runtime not
/// built, or the run's failed step.
pub fn setup(args: &SetupArgs, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    if args.verbose {
        tracing::info!("--verbose given: accepted and ignored, sb has no verbose mode");
    }
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let found = sb_ops::setup::guard(&root, args.force)?;
    let layout = Layout::for_sb();
    let build = BuildInfo::current(env!("CARGO_PKG_VERSION"));
    // uv's and the fact's releases through the proxy or GitHub, as gha
    // reaches them; in a test build, the fake GitHub only.
    let releases = Releases::for_sb(&format!("sb/{}", build.version))?;
    let mut python = Env::for_sb(&layout, &releases, UV_VERSION, Arch::current()?);
    // The apt sources' backup is named by the run log's id; without a log,
    // python-env's own.
    if let Some(log) = log {
        log.id().clone_into(&mut python.run_id);
    }
    let (checks, apt, dirs, locale) = (
        sb_host::setup::Setup::for_sb(),
        Apt::new(),
        Dirs::saltbox(),
        Locale::system(),
    );
    // The completion refresh's shells' directories (a test's root under
    // the `testing` feature) and sb's own executable (M87).
    let (completion_paths, executable) = (
        sb_completion::install::Paths::for_sb(),
        std::env::current_exe(),
    );
    let setup = Setup {
        checks: &checks,
        support: sb_host::check_setup_support,
        apt: &apt,
        dirs: &dirs,
        locale: &locale,
        bash: std::path::Path::new("bash"),
        root: &root,
        url: SALTBOX_URL,
        python: &python,
        run_id: &python.run_id,
        completions: Refresh {
            paths: &completion_paths,
            executable: &executable,
        },
    };
    super::block_on(async {
        // The branch question on the normal screen as before; the local
        // changes question on the alternate screen, as update's (ops
        // decision 42; term-ui's `confirm_over`).
        let ask = |shown: &str, question: &str, flag: &'static str| {
            if flag == BRANCH_FLAGS {
                ask_branch(shown, question, shutdown)
            } else {
                sb_term_ui::confirm_over(
                    question,
                    &super::update::escaped_lines(shown),
                    None,
                    flag,
                    shutdown,
                )
            }
        };
        let plan = sb_ops::setup::plan(
            &root,
            found,
            &args.branch,
            args.branch_flags.answer(),
            args.discard.discard_local,
            ask,
            shutdown,
        )
        .await?;
        let mut runner = Runner::start("sb setup", shutdown, log, Options::default());
        let result = sb_ops::setup::run(&mut runner, &setup, &plan).await;
        runner.finish();
        result.map_err(anyhow::Error::from)
    })?
}

/// A branch question on the terminal, before the runner draws (term-ui's
/// confirm refuses while it does), after repo's line for the current
/// branch, as sb-go prints it first; that line on stderr, where the
/// runner draws, escaped as every line sb prints from git (cli-core's
/// `escaped`). `setup`'s and `update`'s.
///
/// # Errors
///
/// The confirm's: no terminal (naming the branch flags), Ctrl-C.
pub(crate) fn ask_branch(
    shown: &str,
    question: &str,
    shutdown: &Shutdown,
) -> Result<bool, sb_term_ui::PromptError> {
    let _ = writeln!(crate::stderr(), "{}", exit::escaped(shown));
    sb_term_ui::confirm(question, None, BRANCH_FLAGS, shutdown)
}
