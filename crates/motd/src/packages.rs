//! Reboot Status (motd spec, Sections): update-notifier's
//! `/var/run/reboot-required` and `.pkgs`, then its
//! `update-motd-reboot-required` script, as sb-go reads them
//! (`motd/info.go:574-622`). Package Status joins it here with M39.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_exec::{Cmd, Ending};
use tokio_util::sync::CancellationToken;

use crate::Paint;
use crate::text::{block, one_line};

/// sb-go's limit for each child (`motd/utils.go`, `ExecCommand`; motd
/// decision 11).
pub const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

/// update-notifier's files and scripts; the tests point them at a
/// temporary directory (motd spec, Testing, "Children").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// Its flag that a reboot is needed.
    pub reboot_required: PathBuf,
    /// The packages that asked for it, one per line.
    pub reboot_packages: PathBuf,
    /// The script sb-go asks when the flag is absent.
    pub motd_reboot_script: PathBuf,
}

impl Paths {
    /// Where Ubuntu's update-notifier-common puts them.
    #[must_use]
    pub fn system() -> Self {
        Self {
            reboot_required: PathBuf::from("/var/run/reboot-required"),
            reboot_packages: PathBuf::from("/var/run/reboot-required.pkgs"),
            motd_reboot_script: PathBuf::from(
                "/usr/lib/update-notifier/update-motd-reboot-required",
            ),
        }
    }
}

/// Reboot Status, in the warning colour when a reboot is needed:
/// - with the flag file there, `Reboot required (package: P)` for one
///   package in `.pkgs`, `Reboot required (N packages)` for several, and
///   `Reboot required` when the list is empty or can't be read;
/// - without it, the script's output (5 s, exit 0 only), hidden when
///   empty or saying `No reboot`, and in the warning colour when it says
///   `reboot required`;
/// - otherwise nothing, so the line is hidden.
pub async fn reboot(paths: &Paths, paint: &Paint) -> String {
    if paths.reboot_required.exists() {
        return paint.warning(&required(&paths.reboot_packages));
    }
    let output = script_output(&paths.motd_reboot_script).await;
    if output.is_empty() || output.contains("No reboot") {
        return String::new();
    }
    if output.to_lowercase().contains("reboot required") {
        output
            .lines()
            .map(|line| paint.warning(line))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        output
    }
}

/// The flag file's text: the packages from `.pkgs`, read with cli-core's
/// 1 MiB cap.
fn required(packages: &Path) -> String {
    let text = match sb_cli_core::config::read_small(packages) {
        Ok(text) => text.unwrap_or_default(),
        Err(err) => {
            tracing::warn!(file = %packages.display(), "reboot packages not read: {err}");
            String::new()
        }
    };
    let names: Vec<&str> = text.split('\n').filter(|name| !name.is_empty()).collect();
    match names.as_slice() {
        [] => "Reboot required".to_string(),
        [one] => format!("Reboot required (package: {})", one_line(one.as_bytes())),
        many => format!("Reboot required ({} packages)", many.len()),
    }
}

/// The script's stdout, trimmed and made safe to draw, or empty when it
/// can't start, fails or runs past [`CHILD_TIMEOUT`] (sb-go's `Not
/// available`, which it then hides). It runs through sb-exec, in its
/// own process group, ended at the timeout.
async fn script_output(script: &Path) -> String {
    let ran = Cmd::new(script.as_os_str())
        .timeout(CHILD_TIMEOUT)
        .capture(&CancellationToken::new())
        .await;
    match ran {
        Ok(captured) if captured.outcome.ending == Ending::Exited(0) => {
            block(captured.stdout.as_bytes())
        }
        Ok(captured) => {
            tracing::warn!("reboot script {}", captured.outcome.ending);
            String::new()
        }
        Err(err) => {
            tracing::warn!("reboot script not run: {err}");
            String::new()
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Tests that write a stand-in script or start a child hold this from
    /// writing it until the run is over, so no other test's fork holds
    /// the script's write descriptor when it runs (exec fails with
    /// ETXTBSY otherwise), as sb-host's tests do.
    pub(crate) static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Paths in `dir`, none of them there yet.
    fn paths(dir: &Path) -> Paths {
        Paths {
            reboot_required: dir.join("reboot-required"),
            reboot_packages: dir.join("reboot-required.pkgs"),
            motd_reboot_script: dir.join("update-motd-reboot-required"),
        }
    }

    /// A stand-in script printing `output` and exiting `code`.
    fn script(paths: &Paths, body: &str) {
        std::fs::write(&paths.motd_reboot_script, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(
            &paths.motd_reboot_script,
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    fn painted() -> Paint {
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        Paint::new(&theme, true)
    }

    #[test]
    fn the_system_paths_are_update_notifiers() {
        let system = Paths::system();
        assert_eq!(
            system.reboot_required,
            Path::new("/var/run/reboot-required")
        );
        assert_eq!(
            system.reboot_packages,
            Path::new("/var/run/reboot-required.pkgs")
        );
        assert_eq!(
            system.motd_reboot_script,
            Path::new("/usr/lib/update-notifier/update-motd-reboot-required")
        );
    }

    #[tokio::test]
    async fn the_flag_with_one_package_names_it() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "*** System restart required ***\n").unwrap();
        std::fs::write(&paths.reboot_packages, "linux-base\n").unwrap();
        assert_eq!(
            reboot(&paths, &Paint::plain()).await,
            "Reboot required (package: linux-base)"
        );
    }

    #[tokio::test]
    async fn the_flag_with_several_packages_counts_them() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        std::fs::write(&paths.reboot_packages, "linux-base\n\nlibc6\ndbus\n").unwrap();
        assert_eq!(
            reboot(&paths, &Paint::plain()).await,
            "Reboot required (3 packages)"
        );
    }

    /// No list, an empty one or an unreadable one: the plain text.
    #[tokio::test]
    async fn the_flag_without_a_package_list_says_reboot_required() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        assert_eq!(reboot(&paths, &Paint::plain()).await, "Reboot required");
        std::fs::write(&paths.reboot_packages, "\n\n").unwrap();
        assert_eq!(reboot(&paths, &Paint::plain()).await, "Reboot required");
        std::fs::remove_file(&paths.reboot_packages).unwrap();
        std::fs::create_dir(&paths.reboot_packages).unwrap();
        assert_eq!(reboot(&paths, &Paint::plain()).await, "Reboot required");
    }

    #[tokio::test]
    async fn a_package_name_is_made_safe_to_draw() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        std::fs::write(&paths.reboot_packages, "evil\x1b]0;title\x07pkg\n").unwrap();
        assert_eq!(
            reboot(&paths, &Paint::plain()).await,
            "Reboot required (package: evilpkg)"
        );
    }

    #[tokio::test]
    async fn a_needed_reboot_is_in_the_warning_colour() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.reboot_required, "").unwrap();
        let paint = painted();
        assert_eq!(
            reboot(&paths, &paint).await,
            paint.warning("Reboot required")
        );
        assert_ne!(paint.warning("Reboot required"), "Reboot required");
    }

    /// No flag and no script (or one that fails): the line is hidden.
    #[tokio::test]
    async fn no_flag_and_no_answer_hides_the_line() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        assert_eq!(reboot(&paths, &Paint::plain()).await, "");
        script(&paths, "echo 'reboot required'; exit 1");
        assert_eq!(reboot(&paths, &Paint::plain()).await, "");
        script(&paths, "printf '  \\n'");
        assert_eq!(reboot(&paths, &Paint::plain()).await, "");
        script(&paths, "echo 'No reboot needed'");
        assert_eq!(reboot(&paths, &Paint::plain()).await, "");
    }

    /// The script's own text is shown, made safe, in the warning colour
    /// only when it says a reboot is required.
    #[tokio::test]
    async fn the_scripts_text_is_shown_without_the_flag() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        script(
            &paths,
            "printf '\\n*** System Reboot Required ***\\nsecond\\033[2J\\n'",
        );
        let paint = painted();
        assert_eq!(
            reboot(&paths, &paint).await,
            format!(
                "{}\n{}",
                paint.warning("*** System Reboot Required ***"),
                paint.warning("second")
            )
        );
        script(&paths, "echo '*** System restart required ***'");
        assert_eq!(
            reboot(&paths, &paint).await,
            "*** System restart required ***"
        );
    }

    /// A script past the 5 s limit is ended and hides the line.
    #[tokio::test]
    async fn a_hanging_script_is_ended_at_the_limit() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        script(&paths, "echo 'reboot required'; exec sleep 60");
        let start = std::time::Instant::now();
        assert_eq!(reboot(&paths, &Paint::plain()).await, "");
        let took = start.elapsed();
        assert!(took >= CHILD_TIMEOUT, "{took:?}");
        assert!(took < Duration::from_secs(20), "{took:?}");
    }
}
