//! `sb diag` (ansible spec; module build M30): Saltbox's `diag` tag in
//! one attached playbook run, through sb-ansible; `sb install` without
//! the tag cache (M31). `list`, and install's cache and validation, come
//! with their tasks.

use sb_ansible::{Ansible, InstallArgs, Repo, Repos, Routed, RunArgs, Tags};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;

/// `ansible-playbook`: Saltbox's, or `SB_ANSIBLE_PLAYBOOK`'s program in its
/// place (tests and debugging; empty means unset; sudo drops it, as
/// `SB_SALTBOX_DIR`: cli-core spec, amended by the ansible spec). A
/// replacement is logged, as the other overrides are.
fn ansible() -> Ansible {
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

/// `sb install <tags>...` without the tag cache (ansible spec, `sb
/// install`; module build M31): the LXC check, the tags, the disk check
/// (or `--force-disk-full`'s failure), then each repository's run in
/// sb-go's order, the first that fails passed on and the rest not run.
/// Until the cache and validation land (M32, M33), every run goes ahead
/// unvalidated, as `--no-cache` has it, whether or not it is given.
/// exec's forced-exit hook is registered for the runs, as for `diag`.
///
/// # Errors
///
/// LXC, or the LXC check failing; no tags; under 2 GiB free on `/` or the
/// appdata root, or `--force-disk-full`; a playbook that couldn't start,
/// or the first that didn't exit 0 (cli-core's quiet exit with its code).
pub fn install(args: &InstallArgs, shutdown: &Shutdown) -> anyhow::Result<()> {
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
        let paths = [std::path::PathBuf::from("/"), appdata.clone()];
        tokio::task::spawn_blocking(move || sb_host::check_disk_space(&paths)).await??;
        let routed = Routed::route(&tags);
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
