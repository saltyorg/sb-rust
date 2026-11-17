//! The completion refresh `update` and `setup` end with (ops spec, "`sb
//! update`" step 12 and "`sb setup`" step 4; completion spec, "Refresh
//! for `update` and `setup`"; sb-go's `regenerateInstalledCompletions`):
//! completion's `refresh` under a step of its own, a failure only a
//! warning, never the command's failure.

use std::path::PathBuf;

use sb_cli_core::exit::Exit;
use sb_completion::install::Paths;
use sb_term_ui::{Labels, Runner};

/// What the refresh writes under and registers: the shells' directories
/// (completion's `Paths::for_sb`, a test's root under the `testing`
/// feature) and sb's own executable, as `std::env::current_exe` gave it.
#[derive(Debug, Clone, Copy)]
pub struct Refresh<'a> {
    /// Where the shells' files go: `Paths::for_sb`, or a test's root.
    pub paths: &'a Paths,
    /// `std::env::current_exe`'s answer; its error is the refresh's
    /// warning, as completion's `refresh` gives it.
    pub executable: &'a std::io::Result<PathBuf>,
}

/// The refresh as a step of `runner`, "Checking shell completions"
/// (success "Shell completions checked"), holding completion's
/// "Refreshing shell completions" and a step per shell: a failure is its
/// warning under the step, which still succeeds.
///
/// # Errors
///
/// The step's `Exit` only when it doesn't run or is stopped (Ctrl-C, a
/// signal): the run ends by the signal then.
pub async fn refresh(runner: &mut Runner, refresh: &Refresh<'_>) -> Result<(), Exit> {
    runner
        .step(
            Labels::new("Checking shell completions", "Shell completions checked"),
            async |step| {
                sb_completion::refresh(step, refresh.paths, refresh.executable).await;
                Ok(())
            },
        )
        .await
}
