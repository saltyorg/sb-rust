//! `sb diag` (ansible spec; module build M30): Saltbox's `diag` tag in
//! one attached playbook run, through sb-ansible. `install` and `list`
//! come with their tasks.

use sb_ansible::{Ansible, Repo, RunArgs, Tags};
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
