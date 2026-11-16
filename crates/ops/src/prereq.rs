//! `prerequisites`, shared by setup and gha (ops spec, `sb setup` phase
//! 2; sb-go's saltbox/setup.go `InitialSetup`): host's apt calls and the
//! two `/srv` directories, in sb-go's order, each its own step under
//! sb-go's label, its error under sb-go's words.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use sb_host::apt::Apt;
use sb_term_ui::{Labels, Step};

/// The packages sb-go installs last, in its order.
pub const PACKAGES: [&str; 15] = [
    "locales",
    "nano",
    "wget",
    "jq",
    "file",
    "gpg-agent",
    "libpq-dev",
    "build-essential",
    "libssl-dev",
    "libffi-dev",
    "python3-dev",
    "python3-testresources",
    "python3-apt",
    "python3-venv",
    "python3-pip",
];

/// The directories `prerequisites` creates: Saltbox's git directory and
/// the Ansible one (sb-go's `layout.SaltboxGitPath` and
/// `layout.AnsibleVenvPath`).
#[derive(Debug, Clone)]
pub struct Dirs {
    pub git: PathBuf,
    pub ansible: PathBuf,
}

impl Dirs {
    /// `/srv/git` and `/srv/ansible`.
    #[must_use]
    pub fn saltbox() -> Self {
        Self {
            git: PathBuf::from("/srv/git"),
            ansible: PathBuf::from("/srv/ansible"),
        }
    }
}

/// setup's and gha's system prerequisites under `step`: the package
/// lists updated, git and curl installed, [`Dirs`] created (0755 when
/// created, as `os.MkdirAll`), software-properties-common and
/// apt-transport-https installed, host's `add_repositories` (destructive:
/// the old sources move to a root-only backup named by `run_id`, the run
/// log's), the lists updated again, then [`PACKAGES`].
///
/// # Errors
///
/// The failed step's: its block is shown under it, with sb-go's words
/// (`error updating apt cache: ...`) and the cause; the error carries the
/// step's quiet `Exit`.
pub async fn prerequisites(
    step: &mut Step,
    apt: &Apt,
    dirs: &Dirs,
    run_id: &str,
) -> anyhow::Result<()> {
    run(step, "Updating apt package cache", async |step| {
        apt.update_lists(step)
            .await
            .context("error updating apt cache")
    })
    .await?;
    run(step, "Installing git and curl", async |step| {
        apt.install(step, &["git", "curl"])
            .await
            .context("error installing git and curl")
    })
    .await?;
    for dir in [&dirs.git, &dirs.ansible] {
        let label = format!("Creating directory {}", dir.display());
        run(step, &label, async |_| {
            create(dir).with_context(|| format!("error creating {}", dir.display()))
        })
        .await?;
    }
    run(
        step,
        "Installing software-properties-common and apt-transport-https",
        async |step| {
            apt.install(step, &["software-properties-common", "apt-transport-https"])
                .await
                .context("error installing software-properties-common and apt-transport-https")
        },
    )
    .await?;
    run(step, "Adding apt repositories", async |step| {
        apt.add_repositories(step, run_id)
            .await
            .context("error adding apt repositories")
    })
    .await?;
    run(step, "Updating apt package cache again", async |step| {
        apt.update_lists(step)
            .await
            .context("error updating apt cache")
    })
    .await?;
    run(
        step,
        "Installing additional required packages",
        async |step| {
            apt.install(step, &PACKAGES)
                .await
                .context("error installing additional packages")
        },
    )
    .await?;
    Ok(())
}

/// A sub-step of `step` labelled as sb-go's (its running text, also the
/// done text: sb-go's `TaskSpec` gives only `Running`).
async fn run(
    step: &mut Step,
    label: &str,
    body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    step.step(Labels::new(label, label), body).await?;
    Ok(())
}

/// `dir` and its missing parents, 0755 (less the umask) when created, as
/// Go's `os.MkdirAll(dir, 0755)`: std's recursive `DirBuilder`
/// (<https://doc.rust-lang.org/std/fs/struct.DirBuilder.html#method.recursive>).
fn create(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
}
