//! `sb self-update` (release spec, "`sb self-update`"): sb-release's flow
//! on sb's own executable, under term-ui's runner.

use sb_cli_core::build_info::BuildInfo;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::log::RunLog;
use sb_release::{GithubRepo, Outcome, Releases, SelfUpdate};

/// The repository sb's releases come from. Until release preparation
/// decides the channel, sb-go's, the only one there is (release spec,
/// assumption 2): a raw sb-rust build is self-update disabled, so it
/// can't replace itself from it by accident.
const SB_REPO: GithubRepo = GithubRepo {
    owner: "saltyorg",
    name: "sb-go",
};

/// `sb self-update [--yes] [--force-update]`.
pub(crate) fn self_update(
    yes: bool,
    force: bool,
    shutdown: &Shutdown,
    log: Option<&RunLog>,
) -> anyhow::Result<()> {
    let build = BuildInfo::current(env!("CARGO_PKG_VERSION"));
    let target = executable(std::env::current_exe())?;
    let releases = Releases::for_sb(&format!("sb/{}", build.version))?;
    let update = SelfUpdate {
        repo: SB_REPO,
        build: &build,
        target: &target,
        yes,
        force,
        message: None,
        shutdown,
    };
    let outcome = super::block_on(sb_release::self_update(&releases, update, || {
        sb_term_ui::Runner::start(
            "sb self-update",
            shutdown,
            log,
            sb_term_ui::Options::default(),
        )
    }))??;
    if outcome == Outcome::Declined {
        super::warn("Update of sb CLI cancelled");
    }
    Ok(())
}

/// sb's own file, from `/proc/self/exe`
/// (<https://doc.rust-lang.org/std/env/fn.current_exe.html>): refused when
/// it was replaced while sb ran, which the kernel marks ` (deleted)`
/// (proc(5), `/proc/pid/exe`), rather than writing next to a stale path
/// (release spec, known limits).
fn executable(path: std::io::Result<std::path::PathBuf>) -> anyhow::Result<std::path::PathBuf> {
    let path = path.map_err(|err| anyhow::anyhow!("get executable path: {err}"))?;
    if path.to_string_lossy().ends_with(" (deleted)") {
        anyhow::bail!("sb was replaced while running; run it again");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_executable_is_refused() {
        let gone = executable(Ok("/usr/local/bin/sb (deleted)".into()));
        assert_eq!(
            gone.expect_err("replaced").to_string(),
            "sb was replaced while running; run it again"
        );
        let here = executable(Ok("/usr/local/bin/sb".into()));
        assert_eq!(
            here.expect("here"),
            std::path::PathBuf::from("/usr/local/bin/sb")
        );
        let err = executable(Err(std::io::Error::from_raw_os_error(2)));
        assert!(
            err.expect_err("none")
                .to_string()
                .starts_with("get executable path: ")
        );
    }
}
