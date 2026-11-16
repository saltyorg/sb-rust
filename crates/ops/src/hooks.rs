//! `git_hooks`, shared by setup and gha (ops spec, "`git_hooks`"; sb-go's
//! saltbox/setup.go `InitializeGitHooks`): Saltbox's `bin/git/init-hooks`
//! run with bash in the root, when the checkout has it.

use std::path::Path;

use anyhow::{Context as _, anyhow};
use sb_exec::Cmd;
use sb_term_ui::{Labels, Step};

/// The script, under the root.
pub const SCRIPT: &str = "bin/git/init-hooks";

/// `bash <root>/bin/git/init-hooks` in `root`, streamed into a step
/// `Activating Git hooks`; with no script, nothing, silently. `bash` is
/// the program (`bash`, looked up in `PATH`, as sb-go runs it).
///
/// # Errors
///
/// The script there but not inspectable (`error checking for init-hooks
/// script: <cause>`), or the run's failure (`error activating Git hooks:
/// <cause>`, in the step's block; the error then carries its quiet
/// `Exit`).
pub async fn git_hooks(step: &mut Step, root: &Path, bash: &Path) -> anyhow::Result<()> {
    let script = root.join(SCRIPT);
    // `os.Stat`: a link to the script counts, as for sb-go.
    match std::fs::metadata(&script) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(anyhow!("error checking for init-hooks script: {err}")),
        Ok(_) => {}
    }
    let label = "Activating Git hooks";
    step.step(Labels::new(label, label), async |step| {
        // No timeout: Saltbox's script, whose length is its own; Ctrl-C
        // and SIGTERM stop it (as repo's git children).
        let cmd = Cmd::new(bash.as_os_str())
            .shown_value(script.to_string_lossy())
            .cwd(root);
        let ran: anyhow::Result<()> = async {
            let _ = cmd.stream(step).await?.check()?;
            Ok(())
        }
        .await;
        ran.context("error activating Git hooks")
    })
    .await?;
    Ok(())
}
