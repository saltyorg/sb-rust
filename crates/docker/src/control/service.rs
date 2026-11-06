//! The controller's service check (sb-go's `isServiceExistAndRunning`):
//! its unit file, then `systemctl is-active`.

use tokio_util::sync::CancellationToken;

use super::{ControlError, Controller};

/// How much of `systemctl`'s stderr the run log keeps when the service
/// isn't running: its reason (`System has not been booted with systemd`),
/// not a flood.
const STDERR_TAIL: usize = 500;

impl Controller {
    /// Whether the controller's service is there and running: its unit
    /// file must exist (followed through links, as Go's `os.Stat`), then
    /// `systemctl is-active <unit>` must exit 0 and print `active`. It
    /// runs through exec's `capture` with the check's limit and the step's
    /// token, so a cancellation or the limit stops it as exec stops a group
    /// (SIGTERM, the grace, SIGKILL; exec spec, `capture`), not with a
    /// dropped future's SIGKILL. The answer goes to the run log. Why not a
    /// D-Bus crate (zbus): sb-go runs `systemctl`, found on `PATH`, and no
    /// D-Bus client is in sb's tree.
    ///
    /// # Errors
    ///
    /// - [`ControlError::NoService`]: the unit file doesn't exist.
    /// - [`ControlError::Stat`]: it can't be looked at (a path through a
    ///   regular file, a loop of links), `systemctl` not run.
    /// - [`ControlError::NotRunning`]: any other exit, answer or signal, a
    ///   `systemctl` that can't start, or one past the limit.
    /// - [`ControlError::Interrupted`]: the token was cancelled meanwhile.
    pub async fn check_service(&self, token: &CancellationToken) -> Result<(), ControlError> {
        // `fs::exists`: `metadata` (links followed) with ENOENT alone as
        // `false`, as only ENOENT is Go's `ErrNotExist`
        // (<https://doc.rust-lang.org/1.99.1/std/fs/fn.exists.html>).
        match std::fs::exists(&self.unit_file) {
            Ok(true) => {}
            Ok(false) => return Err(ControlError::NoService),
            Err(err) => {
                return Err(ControlError::Stat(sb_cli_core::config::escape_controls(
                    &format!("stat {}: {err}", self.unit_file.display()),
                )));
            }
        }
        // Read before any `check`, branching on the ending (exec spec,
        // decision 9).
        let ran = sb_exec::Cmd::new(self.systemctl.clone())
            .arg("is-active")
            .arg(super::UNIT)
            .timeout(self.check_timeout)
            .capture(token)
            .await;
        let ran = match ran {
            Ok(ran) => ran,
            Err(err) => {
                tracing::info!(
                    cause = %sb_cli_core::config::escape_controls(&err.to_string()),
                    "controller service not running"
                );
                return Err(ControlError::NotRunning);
            }
        };
        let answer = sb_cli_core::config::escape_controls(ran.stdout.trim());
        match ran.outcome.ending {
            sb_exec::Ending::Exited(0) if ran.stdout.trim() == "active" => {
                tracing::info!(answer = %answer, "controller service ready");
                Ok(())
            }
            sb_exec::Ending::Interrupted => Err(ControlError::Interrupted),
            ending => {
                let stderr: String = ran.stderr.trim().chars().take(STDERR_TAIL).collect();
                tracing::info!(
                    outcome = %ending.reason(),
                    answer = %answer,
                    stderr = %sb_cli_core::config::escape_controls(&stderr),
                    "controller service not running"
                );
                Err(ControlError::NotRunning)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::*;
    use crate::control::SPAWNS;

    /// A stand-in `systemctl` in `dir` running `script` (sh): it checks its
    /// arguments are sb-go's, then leaves `ran` behind. Callers hold
    /// `SPAWNS` from writing it until its run is over, so no other test's
    /// fork holds its write descriptor when it runs (ETXTBSY).
    fn systemctl(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("systemctl");
        let ran = dir.join("ran");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n[ \"$1 $2\" = 'is-active {}' ] || exit 9\n: > '{}'\n{script}\n",
                super::super::UNIT,
                ran.display(),
            ),
        )
        .expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    /// A controller whose unit file exists, with `script` as `systemctl`.
    fn controller(dir: &Path, script: &str) -> Controller {
        let unit = dir.join("unit.service");
        std::fs::write(&unit, "").expect("unit");
        Controller::saltbox()
            .expect("client")
            .with_unit_file(unit)
            .with_systemctl(systemctl(dir, script))
            .with_check_timeout(Duration::from_secs(5))
    }

    async fn check(c: &Controller) -> Result<(), ControlError> {
        c.check_service(&CancellationToken::new()).await
    }

    /// Ready only for exit 0 and `active`, whitespace trimmed.
    #[tokio::test]
    async fn active_is_ready() {
        let _spawns = SPAWNS.lock().await;
        for script in ["echo active", "printf '  active \\n\\n'"] {
            let dir = tempfile::tempdir().expect("dir");
            assert_eq!(
                check(&controller(dir.path(), script)).await,
                Ok(()),
                "{script}"
            );
        }
    }

    /// Any other answer, exit, a failure to run it or a timeout is sb-go's
    /// "not running", one plain variant (exec's outcome is logged, not
    /// chained, so the failure block shows sb-go's text alone).
    #[tokio::test]
    async fn anything_else_is_not_running() {
        let _spawns = SPAWNS.lock().await;
        for script in [
            "echo inactive; exit 3",
            "echo active; exit 1",
            "echo inactive",
            "echo activating",
            "kill -KILL $$",
        ] {
            let dir = tempfile::tempdir().expect("dir");
            let err = check(&controller(dir.path(), script))
                .await
                .expect_err(script);
            assert_eq!(err, ControlError::NotRunning, "{script}");
            assert!(dir.path().join("ran").exists(), "{script}: it ran");
        }
        assert_eq!(
            ControlError::NotRunning.to_string(),
            "the Docker controller service is not running"
        );
        // No program there.
        let dir = tempfile::tempdir().expect("dir");
        let c = controller(dir.path(), "echo active")
            .with_systemctl(dir.path().join("no-such-systemctl"));
        assert_eq!(check(&c).await, Err(ControlError::NotRunning));
        // Past the limit: stopped, and "not running" soon after.
        let dir = tempfile::tempdir().expect("dir");
        let c =
            controller(dir.path(), "exec sleep 30").with_check_timeout(Duration::from_millis(200));
        let started = Instant::now();
        assert_eq!(check(&c).await, Err(ControlError::NotRunning));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    /// No unit file, or a link to none (followed, as Go's `os.Stat`):
    /// sb-go's "does not exist", and `systemctl` isn't run.
    #[tokio::test]
    async fn a_missing_unit_file_is_no_service() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let c = controller(dir.path(), "echo active").with_unit_file(dir.path().join("none"));
        assert_eq!(check(&c).await, Err(ControlError::NoService));
        let link = dir.path().join("dangling.service");
        std::os::unix::fs::symlink(dir.path().join("none"), &link).expect("link");
        let c = c.with_unit_file(link);
        assert_eq!(check(&c).await, Err(ControlError::NoService));
        assert!(!dir.path().join("ran").exists());
        assert_eq!(
            ControlError::NoService.to_string(),
            "the Docker controller service does not exist"
        );
    }

    /// Another stat error (a path through a regular file): sb-go's text,
    /// naming the path; `systemctl` isn't run.
    #[tokio::test]
    async fn another_stat_error_names_the_path() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("file");
        std::fs::write(&file, "").expect("file");
        let unit = file.join("unit.service");
        let c = controller(dir.path(), "echo active").with_unit_file(&unit);
        let err = check(&c).await.expect_err("stat");
        assert!(matches!(err, ControlError::Stat(_)), "{err:?}");
        let shown = err.to_string();
        let expected = format!("error checking service status: stat {}: ", unit.display());
        assert!(shown.starts_with(&expected), "{shown}");
        assert!(shown.len() > expected.len(), "{shown}");
        assert!(!dir.path().join("ran").exists());
    }

    /// Cancelled while `systemctl` runs: exec stops it, and the check
    /// passes the interruption on rather than calling it "not running".
    #[tokio::test]
    async fn a_cancelled_check_is_interrupted() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let c = controller(dir.path(), "exec sleep 30");
        let token = CancellationToken::new();
        let cancel = token.clone();
        let ran = dir.path().join("ran");
        tokio::spawn(async move {
            while !ran.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            cancel.cancel();
        });
        let started = Instant::now();
        assert_eq!(
            c.check_service(&token).await,
            Err(ControlError::Interrupted)
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
