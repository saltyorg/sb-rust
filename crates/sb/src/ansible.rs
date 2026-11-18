//! `sb diag` (ansible spec; module build M30): Saltbox's `diag` tag in
//! one attached playbook run, through sb-ansible; `sb install` (M31),
//! its tags validated against the tag cache (M32, M33); `sb list` (M34).

use std::io::Write as _;

use sb_ansible::{
    Ansible, InstallArgs, LazyRunner, ListArgs, Repo, Repos, Routed, RunArgs, TagCache, Tags,
};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::exit::Exit;
use sb_cli_core::log::RunLog;
use sb_cli_core::theme;

/// `ansible-playbook`: Saltbox's, or `SB_ANSIBLE_PLAYBOOK`'s program in its
/// place (tests and debugging; empty means unset; sudo drops it, as
/// `SB_SALTBOX_DIR`: cli-core spec, amended by the ansible spec). A
/// replacement is logged, as the other overrides are.
pub(crate) fn ansible() -> Ansible {
    match super::set_override(std::env::var_os("SB_ANSIBLE_PLAYBOOK")) {
        Some(program) => {
            let program = std::path::PathBuf::from(program);
            tracing::info!(
                variable = "SB_ANSIBLE_PLAYBOOK",
                program = %config::escape_controls(&program.display().to_string()),
                "playbook program overridden"
            );
            Ansible::with_program(program)
        }
        None => Ansible::default(),
    }
}

/// `sb diag`: `ansible-playbook saltbox.yml --become --tags diag`, attached,
/// in the Saltbox repository (`SB_SALTBOX_DIR`'s root), its lines on
/// stdout, its failure passed on with Ansible's own code (sb-go
/// cmd/diag.go; spec, `sb diag`). No LXC or disk check, no cache. exec's
/// forced-exit hook is registered for the run: a second SIGTERM SIGKILLs
/// a playbook still running.
///
/// # Errors
///
/// The playbook couldn't start, or didn't exit 0 (cli-core's quiet exit
/// with its code, after exec's block and the hint).
pub fn diag(shutdown: &Shutdown) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let repo = Repo::saltbox(&root);
    let ansible = ansible();
    let _children = sb_exec::on_forced_exit(shutdown);
    let ran = super::block_on(ansible.run(
        &repo,
        Tags::Literal("diag"),
        &RunArgs::default(),
        shutdown,
        &mut std::io::stdout(),
    ))??;
    sb_ansible::pass_through(ran, &mut anstream::stderr())?;
    Ok(())
}

/// `sb install <tags>...` (ansible spec, `sb install`; module build M31,
/// M33): the LXC check, the tags, the disk check (or `--force-disk-full`'s
/// failure), the tags validated against the cache unless `--no-cache`,
/// then each repository's run in sb-go's order, the first that fails
/// passed on and the rest not run. exec's forced-exit hook is registered
/// for the runs, as for `diag`.
///
/// # Errors
///
/// LXC, or the LXC check failing; no tags; under 2 GiB free on `/` or the
/// appdata root, or `--force-disk-full`; Saltbox's tags missing; a tag
/// with a suggestion (shown, then a quiet exit 1); a playbook that
/// couldn't start, or the first that didn't exit 0 (cli-core's quiet exit
/// with its code).
pub fn install(
    args: &InstallArgs,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let appdata = config::appdata_path(&config::inventory_path(&root));
    let repos = Repos::new(&root, &appdata);
    let ansible = ansible();
    let _children = sb_exec::on_forced_exit(shutdown);
    super::block_on(async {
        // sb-go checks LXC before it reads the tags (cmd/install.go:64).
        sb_host::setup::Setup::new()
            .check_lxc(&shutdown.token())
            .await?;
        let tags = sb_ansible::parse_tags(&args.tags)?;
        if args.force_disk_full {
            return Err(sb_host::disk_space_error(&appdata).into());
        }
        // statvfs blocks on a hung mount: off the runtime's thread, as
        // sb-host asks
        // (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
        // The run's span goes along (cli-core's `spawn`).
        let paths = [std::path::PathBuf::from("/"), appdata.clone()];
        sb_cli_core::spawn::blocking(move || sb_host::check_disk_space(&paths)).await??;
        let routed = Routed::route(&tags);
        // Mod tags alone are never validated, and read no cache (sb-go).
        let validated = !routed.saltbox.is_empty() || !routed.sandbox.is_empty();
        if !args.no_cache && validated {
            check_tags(&routed, &root, &repos, &ansible, shutdown, log).await?;
        }
        let run_args = args.run_args();
        for (repo, tags) in routed.runs(&repos) {
            let ran = ansible
                .run(
                    repo,
                    Tags::Given(tags),
                    &run_args,
                    shutdown,
                    &mut std::io::stdout(),
                )
                .await?;
            sb_ansible::pass_through(ran, &mut anstream::stderr())?;
        }
        Ok(())
    })?
}

/// install's step 5: Saltbox's and Sandbox's tags through the cache, a
/// rebuild a step of a runner titled `sb install` that starts only then,
/// ended before any playbook runs (decision 13); then the suggestions, if
/// any, shown as one error on stderr and a quiet exit 1, nothing run.
///
/// # Errors
///
/// Saltbox's tags missing; an interruption during a rebuild (cli-core
/// then shows it and ends sb by the signal); a suggestion.
async fn check_tags(
    routed: &Routed,
    root: &std::path::Path,
    repos: &Repos,
    ansible: &Ansible,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let mut cache = TagCache::load(root);
    let mut runner = LazyRunner::new(shutdown.token(), || {
        sb_term_ui::Runner::start("sb install", shutdown, log, sb_term_ui::Options::default())
    });
    let known = sb_ansible::known_tags(&mut cache, ansible, repos, &mut runner).await;
    // The runner's drawing ends before any playbook runs (decision 13).
    drop(runner);
    // A Ctrl-C during a rebuild ends the run, Sandbox's lenient failure
    // included: nothing runs (spec, step 8).
    if shutdown.token().is_cancelled() {
        anyhow::bail!("interrupted");
    }
    let suggestions = sb_ansible::validate(routed, &known?);
    if suggestions.is_empty() {
        return Ok(());
    }
    // The run log has the count; the suggestions themselves go to stderr.
    tracing::warn!(
        count = suggestions.len(),
        "tag validation found some issues"
    );
    let pointer = log.map(RunLog::pointer);
    let text = sb_ansible::render_suggestions(&suggestions, theme::current(), pointer.as_deref());
    // As cli-core's error block: nothing better to do if stderr fails.
    let _ = anstream::stderr().write_all(text.as_bytes());
    Err(Exit(1).into())
}

/// `sb list [query]` (ansible spec, `sb list`; module build M34): Saltbox's
/// and Sandbox's tags through the cache, a rebuild a step of a runner
/// titled `sb list` that starts at its first step and ends before any
/// output; `saltbox_mod`'s with `-m`, listed in a step of that runner.
/// Every step is transient (spec, decision 18): on success only the tags
/// are left. Then the refined columns (spec, decision 19), or the search
/// table, on stdout, fitted to its terminal (80 columns when it isn't
/// one) and styled when stdout gets colour, and the repositories that
/// failed as one error after them.
///
/// # Errors
///
/// A repository that failed (exit 1, the others printed); an interruption
/// during a listing (cli-core then shows it and ends sb by the signal);
/// stdout that can't be written.
pub fn list(args: &ListArgs, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let appdata = config::appdata_path(&config::inventory_path(&root));
    let repos = Repos::new(&root, &appdata);
    let ansible = ansible();
    let _children = sb_exec::on_forced_exit(shutdown);
    super::block_on(async {
        let mut cache = TagCache::load(&root);
        let mut runner = LazyRunner::new(shutdown.token(), || {
            sb_term_ui::Runner::start("sb list", shutdown, log, sb_term_ui::Options::default())
        })
        .transient();
        let listing =
            sb_ansible::gather(&mut cache, &ansible, &repos, args.include_mod, &mut runner).await;
        // The steps end before the output starts.
        drop(runner);
        if shutdown.token().is_cancelled() {
            anyhow::bail!("interrupted");
        }
        let width = sb_term_ui::stdout_width();
        // An empty query is none, as in sb-go (`query != ""`).
        let query = args.query.as_deref().filter(|query| !query.is_empty());
        let colour = sb_term_ui::colour_on(sb_term_ui::Stream::Stdout);
        let text = match query {
            // Pure JSON on stdout; the steps drew on stderr (spec, `--json`).
            _ if args.json => Some(sb_ansible::render_list_json(&listing, query)),
            None => Some(sb_ansible::render_list(
                &listing,
                theme::current(),
                colour,
                width.unwrap_or(sb_ansible::PIPED_WIDTH),
            )),
            Some(query) => {
                sb_ansible::render_search(&listing, query, theme::current(), colour, width)
            }
        };
        if let Some(text) = text {
            std::io::stdout()
                .write_all(text.as_bytes())
                .and_then(|()| std::io::stdout().flush())
                .map_err(sb_cli_core::exit::output_error)?;
        }
        match sb_ansible::list_failures(&listing, query.is_some()) {
            Some(err) => Err(err),
            None => Ok(()),
        }
    })?
}
