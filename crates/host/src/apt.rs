//! apt and dpkg (host spec, "apt"; sb-go's host/apt.go). So far the
//! installed-state query, `package_installed`, which the desktop check and
//! python-env's libpq-dev check use.

use std::path::PathBuf;
use std::time::Duration;

use sb_exec::{Cmd, Ending};
use tokio_util::sync::CancellationToken;

/// `package_installed`'s limit outside the desktop check (host spec,
/// difference 7).
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// A package's state couldn't be read, in sb-go's words.
#[derive(Debug, thiserror::Error)]
#[error("query package {name}: {cause}")]
pub struct PackageQueryError {
    name: String,
    cause: String,
}

/// apt and dpkg, as sb runs them.
#[derive(Debug, Clone)]
pub struct Apt {
    dpkg_query: PathBuf,
}

impl Default for Apt {
    fn default() -> Self {
        Self::new()
    }
}

impl Apt {
    /// The programs from `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dpkg_query: PathBuf::from("dpkg-query"),
        }
    }

    /// Run `program` as dpkg-query instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_dpkg_query(mut self, program: impl Into<PathBuf>) -> Self {
        self.dpkg_query = program.into();
        self
    }

    /// Whether dpkg has `name` installed and configured (`ii`), within
    /// 10 s (sb-go's `IsPackageInstalled`).
    ///
    /// # Errors
    ///
    /// [`PackageQueryError`] when dpkg-query can't run, times out, is
    /// interrupted or exits with anything but 0 or 1. Exit 1 (an unknown
    /// or never-installed package) is `Ok(false)`.
    pub async fn package_installed(
        &self,
        name: &str,
        token: &CancellationToken,
    ) -> Result<bool, PackageQueryError> {
        self.installed_within(name, QUERY_TIMEOUT, token).await
    }

    /// [`Apt::package_installed`] with `limit` in place of 10 s (the
    /// desktop check keeps sb-go's 5 s).
    pub(crate) async fn installed_within(
        &self,
        name: &str,
        limit: Duration,
        token: &CancellationToken,
    ) -> Result<bool, PackageQueryError> {
        let failed = |cause: String| PackageQueryError {
            name: name.to_string(),
            cause,
        };
        let ran = Cmd::new(self.dpkg_query.as_os_str())
            .arg("-W")
            .arg("-f=${db:Status-Abbrev}")
            .value(name)
            .timeout(limit)
            .capture(token)
            .await
            .map_err(|err| failed(format!("{err}: {}", err.cause())))?;
        if ran.outcome.ending == Ending::Exited(1) {
            return Ok(false);
        }
        ran.check()
            .map(|ran| installed(&ran.stdout))
            .map_err(|failure| failed(crate::child_failure(&failure)))
    }
}

/// sb-go's desktop rule on dpkg's abbreviated status: only an installed
/// package (`ii`) counts; `rc` (removed, its configuration kept) and `un`
/// don't (host spec, difference 2).
fn installed(status: &str) -> bool {
    status.trim() == "ii"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{SPAWNS, stand_in};

    #[test]
    fn only_ii_is_installed() {
        assert!(installed("ii "));
        assert!(installed("ii"));
        for status in ["rc ", "un ", "iU ", "hi ", ""] {
            assert!(!installed(status), "{status:?}");
        }
    }

    async fn query(script: &str, name: &str) -> Result<bool, PackageQueryError> {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(dir.path(), "dpkg-query", script);
        Apt::new()
            .with_dpkg_query(program)
            .package_installed(name, &CancellationToken::new())
            .await
    }

    #[tokio::test]
    async fn an_installed_package_is_true() {
        let _spawns = SPAWNS.lock().await;
        let script = r#"[ "$*" = '-W -f=${db:Status-Abbrev} libpq-dev' ] || exit 9; printf 'ii '"#;
        assert!(query(script, "libpq-dev").await.expect("query"));
    }

    #[tokio::test]
    async fn a_removed_package_is_false() {
        let _spawns = SPAWNS.lock().await;
        assert!(
            !query("printf 'rc '", "ubuntu-desktop")
                .await
                .expect("query")
        );
    }

    #[tokio::test]
    async fn exit_1_is_false() {
        let _spawns = SPAWNS.lock().await;
        let script = "echo 'dpkg-query: no packages found matching x' >&2; exit 1";
        assert!(!query(script, "x").await.expect("query"));
    }

    #[tokio::test]
    async fn exit_2_is_the_query_error() {
        let _spawns = SPAWNS.lock().await;
        let error = query("echo 'dpkg-query: error' >&2; exit 2", "x")
            .await
            .expect_err("failed");
        let text = error.to_string();
        assert!(text.starts_with("query package x: "), "{text}");
        assert!(text.contains("exited with code 2"), "{text}");
    }

    #[tokio::test]
    async fn a_hung_query_times_out() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(dir.path(), "dpkg-query", "exec sleep 60");
        let error = Apt::new()
            .with_dpkg_query(program)
            .installed_within("x", Duration::from_millis(200), &CancellationToken::new())
            .await
            .expect_err("timed out");
        assert!(error.to_string().contains("timed out"), "{error}");
    }

    #[tokio::test]
    async fn a_missing_program_is_the_query_error() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let error = Apt::new()
            .with_dpkg_query(dir.path().join("missing"))
            .package_installed("x", &CancellationToken::new())
            .await
            .expect_err("failed");
        assert!(
            error
                .to_string()
                .starts_with("query package x: could not run "),
            "{error}"
        );
    }

    #[test]
    fn the_default_limit_is_ten_seconds() {
        assert_eq!(QUERY_TIMEOUT, Duration::from_secs(10));
    }
}
