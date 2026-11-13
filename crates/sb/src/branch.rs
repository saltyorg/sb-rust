//! `sb branch-sandbox <name>`, bare (module build M5): the Sandbox
//! checkout switched to a branch with sb-repo, owned by the Saltbox user,
//! as the ops spec's steps 1 to 3 have it. No tags cache yet; ops replaces
//! this wiring with its branch commands (M79).

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config;
use sb_cli_core::log::RunLog;
use sb_repo::Repo;
use sb_term_ui::{Labels, Options, Runner};

/// The checkout's name in labels and errors.
const SANDBOX: &str = "Sandbox";

/// `sb branch-sandbox <name>`: the branch name checked, the Saltbox user
/// read (config's `saltbox_user`), then the checkout at
/// `<server_appdata_path>/sandbox` (the appdata path from the inventory
/// under `SB_SALTBOX_DIR`'s root, as sb finds its theme) fetched and reset
/// to `name` under sb-go's labels.
///
/// # Errors
///
/// The checkout missing or not a directory, a refused branch name or a
/// missing Saltbox user (before anything changes, as sb's error block),
/// or the switch's failed step.
pub fn branch_sandbox(name: &str, shutdown: &Shutdown, log: Option<&RunLog>) -> anyhow::Result<()> {
    let root = config::saltbox_dir(std::env::var_os("SB_SALTBOX_DIR"));
    let appdata = config::appdata_path(&config::inventory_path(&root));
    let repo = Repo::open(SANDBOX, appdata.join("sandbox"))?;
    super::block_on(async {
        repo.check_branch_name(name, &shutdown.token()).await?;
        let owner = sb_config::user::saltbox_user(&root)?;
        let mut runner = Runner::start("sb branch-sandbox", shutdown, log, Options::default());
        let result = switch(&mut runner, &repo, name, &owner).await;
        runner.finish();
        result.map_err(anyhow::Error::from)
    })?
}

/// The switch under sb-go's labels (cmd/branchSandbox.go).
async fn switch(
    runner: &mut Runner,
    repo: &Repo,
    branch: &str,
    owner: &str,
) -> Result<(), sb_cli_core::exit::Exit> {
    let labels = Labels::new(
        format!("Switching {SANDBOX} repository to {branch}"),
        format!("{SANDBOX} repository switched to {branch}"),
    )
    .failure(format!("{SANDBOX} branch switch"));
    runner
        .step(labels, async |step| {
            let updating = Labels::new(
                format!("Updating {SANDBOX} repository"),
                format!("{SANDBOX} repository updated ({branch})"),
            )
            .failure(format!("{SANDBOX} repository update"));
            step.step(updating, async |step| {
                repo.fetch_and_reset(branch, owner, step).await
            })
            .await?;
            Ok(())
        })
        .await
}
