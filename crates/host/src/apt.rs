//! apt and dpkg (host spec, "apt"; sb-go's host/apt.go): the installed-state
//! query, `package_installed`, which the desktop check and python-env's
//! libpq-dev check use; and apt-get under sb-go's policy, `install`,
//! `remove`, `autoremove` and `update_lists`, streamed into the caller's
//! step.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use sb_exec::{Cmd, Ending};
use sb_term_ui::Step;
use tokio_util::sync::CancellationToken;

mod sources;

pub use sources::SourcesError;

/// `package_installed`'s limit outside the desktop check (host spec,
/// difference 7).
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// apt's own wait for the dpkg frontend lock on install, remove and
/// autoremove (host spec, decision 3 and difference 1): apt retries the
/// lock once a second, printing `Waiting for cache lock: <holder>`, for
/// up to 240 s. Verified on apt 2.4, 2.8 and 3.2 against an `fcntl` lock
/// (docs/platform-notes/apt.md).
const LOCK_TIMEOUT: &str = "DPkg::Lock::Timeout=240";

/// `lsb_release`'s limit (host spec, difference 7).
const CODENAME_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times `update_lists` runs `apt-get update` for a timeout or a
/// mirror error (sb-go's `UpdatePackageLists`).
const ATTEMPTS: u32 = 3;

/// The mirror errors worth another attempt, in apt's stderr (sb-go's).
const MIRROR_ERRORS: [&str; 3] = [
    "Mirror sync in progress",
    "File has unexpected size",
    "Hashes of expected file",
];

/// apt's error when another process holds the package lists' lock, which
/// `DPkg::Lock::Timeout` doesn't cover (host spec, decision 5). Checked on
/// apt 2.4, 2.8 and 3.2: on stderr, at once (docs/platform-notes/apt.md).
const LISTS_LOCKED: &str = "Could not get lock /var/lib/apt/lists/lock";

/// `update_lists`' times.
#[derive(Debug, Clone, Copy)]
struct UpdateTiming {
    /// Each attempt's limit.
    attempt: Duration,
    /// The first delay before another attempt; it doubles.
    delay: Duration,
    /// The wait before another try on a locked package lists.
    lock_retry: Duration,
    /// The total wait for a locked package lists.
    lock_budget: Duration,
}

/// sb-go's times, and the dpkg lock's 240 s for the lists lock.
const UPDATE_TIMING: UpdateTiming = UpdateTiming {
    attempt: Duration::from_secs(120),
    delay: Duration::from_secs(5),
    lock_retry: Duration::from_secs(5),
    lock_budget: Duration::from_secs(240),
};

/// A package's state couldn't be read, in sb-go's words.
#[derive(Debug, thiserror::Error)]
#[error("query package {name}: {cause}")]
pub struct PackageQueryError {
    name: String,
    cause: String,
}

/// `package_list`'s format: four tab-separated fields a row, in dpkg's own
/// escapes, which dpkg-query expands (python-env amendment H2).
const LIST_FORMAT: &str =
    r"-f=${binary:Package}\t${source:Package}\t${db:Status-Abbrev}\t${Version}\n";

/// One package in dpkg's database, as [`Apt::package_list`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRow {
    /// `${binary:Package}`: the name, with its architecture when it is
    /// multi-arch (`libc6:amd64`).
    pub binary: String,
    /// `${source:Package}`: its source package.
    pub source: String,
    /// `${db:Status-Abbrev}` without its trailing blank: `ii`, `rc`,
    /// `un`, ... (a third letter is dpkg's error flag).
    pub status: String,
    /// `${Version}`; empty for a package that isn't installed.
    pub version: String,
}

/// The package list couldn't be read (python-env amendment H2).
#[derive(Debug, thiserror::Error)]
#[error("list packages: {cause}")]
pub struct PackageListError {
    cause: String,
}

/// A package's apt policy couldn't be read (python-env amendment H2).
#[derive(Debug, thiserror::Error)]
#[error("query package policy {name}: {cause}")]
pub struct PackagePolicyError {
    name: String,
    cause: String,
}

/// apt and dpkg, as sb runs them.
#[derive(Debug, Clone)]
pub struct Apt {
    dpkg_query: PathBuf,
    /// apt-get's program (a field named `apt_get` would repeat the type's
    /// name).
    get_program: PathBuf,
    /// apt-cache's program.
    cache_program: PathBuf,
    /// `package_list`'s and `package_policy`'s limit.
    query_timeout: Duration,
    update: UpdateTiming,
    lsb_release: PathBuf,
    codename_timeout: Duration,
    sources: sources::Roots,
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
            get_program: PathBuf::from("apt-get"),
            cache_program: PathBuf::from("apt-cache"),
            query_timeout: QUERY_TIMEOUT,
            update: UPDATE_TIMING,
            lsb_release: PathBuf::from("lsb_release"),
            codename_timeout: CODENAME_TIMEOUT,
            sources: sources::Roots::default(),
        }
    }

    /// Run `program` as dpkg-query instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_dpkg_query(mut self, program: impl Into<PathBuf>) -> Self {
        self.dpkg_query = program.into();
        self
    }

    /// Run `program` as apt-get instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_apt_get(mut self, program: impl Into<PathBuf>) -> Self {
        self.get_program = program.into();
        self
    }

    /// Run `program` as apt-cache instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_apt_cache(mut self, program: impl Into<PathBuf>) -> Self {
        self.cache_program = program.into();
        self
    }

    /// `update_lists` with these times in place of 2 min attempts, 5 s
    /// and 10 s delays, and a 5 s lists-lock retry within 240 s (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_update_timing(
        mut self,
        attempt: Duration,
        delay: Duration,
        lock_retry: Duration,
        lock_budget: Duration,
    ) -> Self {
        self.update = UpdateTiming {
            attempt,
            delay,
            lock_retry,
            lock_budget,
        };
        self
    }

    /// Run `program` as `lsb_release` instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_lsb_release(mut self, program: impl Into<PathBuf>) -> Self {
        self.lsb_release = program.into();
        self
    }

    /// `add_repositories` and `remove_sources_mentioning` on `etc_apt` and
    /// `var_backups` in place of `/etc/apt` and `/var/backups`, with
    /// directories owned by `owner` in place of root (tests; the real
    /// paths are never arguments).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_sources_roots(
        mut self,
        etc_apt: impl Into<PathBuf>,
        var_backups: impl Into<PathBuf>,
        owner: u32,
    ) -> Self {
        self.sources = sources::Roots {
            etc_apt: etc_apt.into(),
            var_backups: var_backups.into(),
            owner,
            ..sources::Roots::default()
        };
        self
    }

    /// Moves the apt sources that mention `needle` into a root-only backup.
    ///
    /// Each regular file directly in `/etc/apt/sources.list.d` whose
    /// content contains `needle`, and each symlink there whose target's
    /// content does, goes into `/var/backups/sb/<backup>/<run id>/`, and
    /// the result says where each now is (host spec, "apt",
    /// `remove_sources_mentioning`, amendment H3; python-env passes
    /// `deadsnakes` for both). A link moves as a link: its target is read
    /// through no other link, at most 1 MiB of it, and never written or
    /// moved. `needle` and `backup` are written in sb's code, never input;
    /// `run_id` is the run log's, as for [`Apt::add_repositories`].
    /// Subdirectories, and links that reach no regular file in one step
    /// (dangling, chained, to a directory or a device), are left alone;
    /// each name is logged before it moves. The files are a few small
    /// ones, so the work runs on the caller's thread.
    ///
    /// # Errors
    ///
    /// [`SourcesError`]: a refused directory, an existing run directory,
    /// a bad run id or folder, a file that couldn't be read or moved, or a
    /// directory that couldn't be synced afterwards. Nothing has moved
    /// unless the error is a move's or a sync's; each move before it is in
    /// the run log.
    pub fn remove_sources_mentioning(
        &self,
        needle: &'static str,
        backup: &'static str,
        run_id: &str,
    ) -> Result<Vec<PathBuf>, SourcesError> {
        sources::remove_mentioning(&self.sources, needle, backup, run_id)
    }

    /// The apt sources that still mention `needle`, read-only.
    ///
    /// Each regular file directly in `/etc/apt/sources.list.d`, and each
    /// symlink there, that [`Apt::remove_sources_mentioning`] would move,
    /// by the same rule and checks, as its path there, in name order:
    /// python-env's re-check after its deadsnakes cleanup (host spec,
    /// "apt", `sources_mentioning`, amended 2026-11-15). Nothing is
    /// created or moved; an empty `needle` and a missing directory name
    /// nothing. The paths hold the names as they are: escape them to show
    /// them. The files are a few small ones, so the work runs on the
    /// caller's thread.
    ///
    /// # Errors
    ///
    /// [`SourcesError`]: a refused `sources.list.d`, or an entry (or a
    /// link's target) that couldn't be read.
    pub fn sources_mentioning(&self, needle: &'static str) -> Result<Vec<PathBuf>, SourcesError> {
        sources::sources_mentioning(&self.sources, needle)
    }

    /// Saltbox's apt repositories for this release (host spec, "apt",
    /// `add_repositories`): the codename from `lsb_release -sc`'s stdout
    /// (5 s), then [`sources`]' rules, with the moved entries' count and
    /// backup directory in `step`. `run_id` is the run log's, naming the
    /// backup directory. The files are a few small ones, so the work runs
    /// on this task.
    ///
    /// # Errors
    ///
    /// [`SourcesError`]: the codename, an unsupported release, a refused
    /// directory, or a file that couldn't be moved, read or written.
    pub async fn add_repositories(
        &self,
        step: &mut Step,
        run_id: &str,
    ) -> Result<(), SourcesError> {
        let codename = self.codename(&step.token()).await?;
        let mut report = sources::Report::default();
        let applied = sources::apply(&self.sources, &codename, run_id, &mut report);
        // Where the entries went, even when a later part failed.
        if let Some(backup) = report.backup.as_ref().filter(|_| !report.moved.is_empty()) {
            step.info(format!(
                "Moved {} entries from {} to {}",
                report.moved.len(),
                self.sources.etc_apt.join("sources.list.d").display(),
                backup.display()
            ));
        }
        applied
    }

    /// `lsb_release -sc`'s stdout, trimmed (difference 3: not stderr).
    async fn codename(&self, token: &CancellationToken) -> Result<String, SourcesError> {
        let ran = Cmd::new(self.lsb_release.as_os_str())
            .arg("-sc")
            .timeout(self.codename_timeout)
            .capture(token)
            .await
            .map_err(|err| SourcesError::Codename(format!("{err}: {}", err.cause())))?;
        let ran = ran
            .check()
            .map_err(|failed| SourcesError::Codename(crate::child_failure(&failed)))?;
        Ok(ran.stdout.trim().to_string())
    }

    /// apt-get under sb-go's policy: the two conffile options, and
    /// `DEBIAN_FRONTEND=noninteractive` and `NEEDRESTART_MODE=l` added to
    /// the inherited environment (host spec, "apt").
    fn apt_get(&self) -> Cmd {
        Cmd::new(self.get_program.as_os_str())
            .arg("-o")
            .arg("Dpkg::Options::=--force-confdef")
            .arg("-o")
            .arg("Dpkg::Options::=--force-confold")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("NEEDRESTART_MODE", "l")
    }

    /// [`Apt::apt_get`] with apt's wait for the dpkg lock.
    fn apt_get_waiting(&self) -> Cmd {
        self.apt_get().arg("-o").arg(LOCK_TIMEOUT)
    }

    /// `apt-get install -y <packages>` (sb's own package names, never a
    /// user's: no `--` before them, as sb-go), apt waiting up to 240 s for the
    /// dpkg lock, streamed into `step` with no timeout (an install's
    /// length depends on the packages and the mirror; Ctrl-C stops it).
    ///
    /// # Errors
    ///
    /// exec's spawn error or `Failed`, under sb-go's `failed to install
    /// packages '<a, b>'`.
    pub async fn install(&self, step: &mut Step, packages: &[&str]) -> anyhow::Result<()> {
        let cmd = self.apt_get_waiting().arg("install").arg("-y");
        run(step, with_values(cmd, packages))
            .await
            .with_context(|| format!("failed to install packages '{}'", packages.join(", ")))
    }

    /// `apt-get remove -y <packages>`, as [`Apt::install`] runs (python-env's
    /// deadsnakes cleanup, which adds its own text).
    ///
    /// # Errors
    ///
    /// exec's spawn error or `Failed`.
    pub async fn remove(&self, step: &mut Step, packages: &[&str]) -> anyhow::Result<()> {
        let cmd = self.apt_get_waiting().arg("remove").arg("-y");
        run(step, with_values(cmd, packages)).await
    }

    /// `apt-get autoremove -y`, as [`Apt::install`] runs.
    ///
    /// # Errors
    ///
    /// exec's spawn error or `Failed`.
    pub async fn autoremove(&self, step: &mut Step) -> anyhow::Result<()> {
        run(step, self.apt_get_waiting().arg("autoremove").arg("-y")).await
    }

    /// `apt-get update` with sb-go's retries (`UpdatePackageLists`),
    /// streamed into `step`:
    /// - up to 3 attempts of 2 min each, another after a timeout or a
    ///   mirror error in apt's stderr ([`MIRROR_ERRORS`]), 5 s and then
    ///   10 s later, with sb-go's line in the step;
    /// - apt's lists-lock error tried again every 5 s within 240 s,
    ///   outside the attempts (decision 5): `DPkg::Lock::Timeout` doesn't
    ///   cover `/var/lib/apt/lists/lock`;
    /// - an interrupted attempt is never tried again.
    ///
    /// # Errors
    ///
    /// The last attempt's error, or `context cancelled during retry:
    /// interrupted` when the step is cancelled while waiting, under
    /// sb-go's `failed to update package lists`.
    pub async fn update_lists(&self, step: &mut Step) -> anyhow::Result<()> {
        self.update_attempts(step)
            .await
            .context("failed to update package lists")
    }

    /// Why not backon (<https://docs.rs/backon/1.6.0/backon/>): not in the
    /// tree, and the lists-lock retry runs on its own budget outside the
    /// attempts, with sb-go's numbered lines and the step's token.
    async fn update_attempts(&self, step: &mut Step) -> anyhow::Result<()> {
        let timing = self.update;
        let token = step.token();
        let (mut attempt, mut delay, mut lock_waited) = (1, timing.delay, Duration::ZERO);
        loop {
            let ran = self
                .apt_get()
                .arg("update")
                .timeout(timing.attempt)
                .stream(step)
                .await?;
            let decision = next(ran.outcome.ending, &ran.stderr_tail);
            let Err(failed) = ran.check() else {
                return Ok(());
            };
            let line = match decision {
                Next::ListsLocked if lock_waited < timing.lock_budget => {
                    tracing::info!(
                        waited_ms = lock_waited.as_millis(),
                        budget_ms = timing.lock_budget.as_millis(),
                        "apt-get update: the package lists are locked, trying again"
                    );
                    wait(&token, timing.lock_retry).await?;
                    lock_waited += timing.lock_retry;
                    continue;
                }
                Next::TimedOut if attempt < ATTEMPTS => format!(
                    "apt-get update timed out after {} (attempt {attempt}/{ATTEMPTS}), retrying in {}...",
                    go_duration(timing.attempt),
                    go_duration(delay)
                ),
                Next::MirrorError if attempt < ATTEMPTS => format!(
                    "apt-get update failed (attempt {attempt}/{ATTEMPTS}), retrying in {} due to \
                     transient mirror sync error...",
                    go_duration(delay)
                ),
                _ => return Err(failed.into()),
            };
            step.info(line);
            wait(&token, delay).await?;
            delay *= 2;
            attempt += 1;
        }
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

    /// Every package in dpkg's database, one [`PackageRow`] each, for
    /// python-env's deadsnakes cleanup (host spec, "apt", amendment H2):
    /// `dpkg-query -W` with [`LIST_FORMAT`], captured, 10 s.
    ///
    /// # Errors
    ///
    /// [`PackageListError`] when dpkg-query can't run, times out, is
    /// interrupted or fails; when its output was cut at exec's capture
    /// limit (a row could be missing); or when a row doesn't hold four
    /// fields.
    pub async fn package_list(
        &self,
        token: &CancellationToken,
    ) -> Result<Vec<PackageRow>, PackageListError> {
        let failed = |cause: String| PackageListError { cause };
        let ran = Cmd::new(self.dpkg_query.as_os_str())
            .arg("-W")
            .arg(LIST_FORMAT)
            .timeout(self.query_timeout)
            .capture(token)
            .await
            .map_err(|err| failed(format!("{err}: {}", err.cause())))?
            .check()
            .map_err(|failure| failed(crate::child_failure(&failure)))?;
        if ran.truncated {
            return Err(failed(
                "dpkg-query's output was cut at exec's limit".to_string(),
            ));
        }
        ran.stdout
            .lines()
            .map(|line| {
                package_row(line)
                    .ok_or_else(|| failed(format!("unexpected row \"{}\"", line.escape_debug())))
            })
            .collect()
    }

    /// `apt-cache policy <name>`'s text, for python-env to parse (host
    /// spec, "apt", amendment H2): captured, 10 s, with `LC_ALL=C` so the
    /// `***` and origin lines read the same whatever the locale. An
    /// unknown package is apt's empty answer, not an error.
    ///
    /// # Errors
    ///
    /// [`PackagePolicyError`] when apt-cache can't run, times out, is
    /// interrupted or fails.
    pub async fn package_policy(
        &self,
        name: &str,
        token: &CancellationToken,
    ) -> Result<String, PackagePolicyError> {
        let failed = |cause: String| PackagePolicyError {
            name: name.to_string(),
            cause,
        };
        let ran = Cmd::new(self.cache_program.as_os_str())
            .arg("policy")
            // The name can never be read as an option (apt 2.4 honours `--`).
            .arg("--")
            .value(name)
            .env("LC_ALL", "C")
            .timeout(self.query_timeout)
            .capture(token)
            .await
            .map_err(|err| failed(format!("{err}: {}", err.cause())))?
            .check()
            .map_err(|failure| failed(crate::child_failure(&failure)))?;
        Ok(ran.stdout)
    }
}

/// A [`LIST_FORMAT`] row: exactly four tab-separated fields, the status's
/// trailing blank trimmed.
fn package_row(line: &str) -> Option<PackageRow> {
    let mut fields = line.split('\t');
    let row = PackageRow {
        binary: fields.next()?.to_string(),
        source: fields.next()?.to_string(),
        status: fields.next()?.trim_end().to_string(),
        version: fields.next()?.to_string(),
    };
    fields.next().is_none().then_some(row)
}

/// sb-go's desktop rule on dpkg's abbreviated status: only an installed
/// package (`ii`) counts; `rc` (removed, its configuration kept) and `un`
/// don't (host spec, difference 2).
fn installed(status: &str) -> bool {
    status.trim() == "ii"
}

/// What `update_lists` does after a failed `apt-get update`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// Another process holds the package lists' lock: try again, outside
    /// the attempts (decision 5).
    ListsLocked,
    /// The attempt timed out: another attempt.
    TimedOut,
    /// A transient mirror error: another attempt.
    MirrorError,
    /// Fail: any other error, and an interrupted or killed attempt, which
    /// is never retried.
    Fail,
}

/// The decision for an attempt that ended so, with this stderr.
fn next(ending: Ending, stderr: &str) -> Next {
    match ending {
        Ending::TimedOut => Next::TimedOut,
        Ending::Exited(code) if code != 0 => {
            if stderr.contains(LISTS_LOCKED) {
                Next::ListsLocked
            } else if MIRROR_ERRORS.iter().any(|text| stderr.contains(text)) {
                Next::MirrorError
            } else {
                Next::Fail
            }
        }
        _ => Next::Fail,
    }
}

/// A duration as Go's `time.Duration` prints a whole number of seconds or
/// milliseconds (`2m0s`, `5s`, `200ms`), for sb-go's retry lines. Why not
/// jiff's friendly format
/// (<https://docs.rs/jiff/0.2.38/jiff/fmt/friendly/struct.SpanPrinter.html>):
/// it leaves out a zero unit, so `2m`, not Go's `2m0s`.
fn go_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match (seconds / 60, seconds % 60) {
        (0, 0) => format!("{}ms", duration.as_millis()),
        (0, seconds) => format!("{seconds}s"),
        (minutes, seconds) => format!("{minutes}m{seconds}s"),
    }
}

/// `cmd` with `values` after its literal arguments.
fn with_values(cmd: Cmd, values: &[&str]) -> Cmd {
    values.iter().fold(cmd, |cmd, value| cmd.value(*value))
}

/// `cmd` streamed into `step` with no timeout; its output is in the step
/// and the run log.
async fn run(step: &mut Step, cmd: Cmd) -> anyhow::Result<()> {
    let _streamed = cmd.stream(step).await?.check()?;
    Ok(())
}

/// Wait `delay`, or fail if `token` is cancelled first (sb-go's
/// `context cancelled during retry`), with tokio-util's
/// [`CancellationToken::run_until_cancelled`]
/// (<https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled>).
async fn wait(token: &CancellationToken, delay: Duration) -> anyhow::Result<()> {
    token
        .run_until_cancelled(tokio::time::sleep(delay))
        .await
        .ok_or_else(|| anyhow::anyhow!("context cancelled during retry: interrupted"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::testing::{SPAWNS, in_step, stand_in};

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
        assert_eq!(Apt::new().query_timeout, QUERY_TIMEOUT);
    }

    async fn list(script: &str) -> Result<Vec<PackageRow>, PackageListError> {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(dir.path(), "dpkg-query", script);
        Apt::new()
            .with_dpkg_query(program)
            .package_list(&CancellationToken::new())
            .await
    }

    fn row(binary: &str, source: &str, status: &str, version: &str) -> PackageRow {
        PackageRow {
            binary: binary.to_string(),
            source: source.to_string(),
            status: status.to_string(),
            version: version.to_string(),
        }
    }

    #[tokio::test]
    async fn package_list_reads_four_fields_a_row() {
        let _spawns = SPAWNS.lock().await;
        // The format reaches dpkg-query with its `\t` and `\n` escapes,
        // which dpkg expands (checked on dpkg 1.21).
        let script = r#"[ "$*" = '-W -f=${binary:Package}\t${source:Package}\t${db:Status-Abbrev}\t${Version}\n' ] || exit 9
printf 'python3.12\tpython3.12\tii \t3.12.1-1+jammy1\n'
printf 'libc6:amd64\tglibc\tii \t2.35-0ubuntu3\n'
printf 'old-tool\told-tool\trc \t1.0\n'
printf 'gone\tgone\tun \t\n'"#;
        assert_eq!(
            list(script).await.expect("list"),
            [
                row("python3.12", "python3.12", "ii", "3.12.1-1+jammy1"),
                row("libc6:amd64", "glibc", "ii", "2.35-0ubuntu3"),
                row("old-tool", "old-tool", "rc", "1.0"),
                row("gone", "gone", "un", ""),
            ]
        );
    }

    #[tokio::test]
    async fn a_row_without_four_fields_is_the_list_error() {
        let _spawns = SPAWNS.lock().await;
        let error = list(r"printf 'a\ta\tii \t1\nb\tb\tii\n'")
            .await
            .expect_err("bad row");
        assert_eq!(
            error.to_string(),
            r#"list packages: unexpected row "b\tb\tii""#
        );
        let error = list(r"printf 'a\ta\tii \t1\textra\n'")
            .await
            .expect_err("bad row");
        assert_eq!(
            error.to_string(),
            r#"list packages: unexpected row "a\ta\tii \t1\textra""#
        );
    }

    #[tokio::test]
    async fn a_list_cut_at_exec_s_cap_is_the_list_error() {
        let _spawns = SPAWNS.lock().await;
        let error = list("yes 'a\ta\tii \t1' | head -c 2000000")
            .await
            .expect_err("cut");
        assert_eq!(
            error.to_string(),
            "list packages: dpkg-query's output was cut at exec's limit"
        );
    }

    #[tokio::test]
    async fn a_failed_list_is_the_list_error() {
        let _spawns = SPAWNS.lock().await;
        let error = list("echo 'dpkg-query: error' >&2; exit 2")
            .await
            .expect_err("failed");
        let text = error.to_string();
        assert!(text.starts_with("list packages: "), "{text}");
        assert!(
            text.contains("exited with code 2") && text.contains("dpkg-query: error"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_hung_list_times_out() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut apt =
            Apt::new().with_dpkg_query(stand_in(dir.path(), "dpkg-query", "exec sleep 60"));
        apt.query_timeout = Duration::from_millis(200);
        let error = apt
            .package_list(&CancellationToken::new())
            .await
            .expect_err("timed out");
        let text = error.to_string();
        assert!(
            text.starts_with("list packages: ") && text.contains("timed out"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_missing_dpkg_query_is_the_list_error() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let error = Apt::new()
            .with_dpkg_query(dir.path().join("missing"))
            .package_list(&CancellationToken::new())
            .await
            .expect_err("failed");
        assert!(
            error
                .to_string()
                .starts_with("list packages: could not run "),
            "{error}"
        );
    }

    async fn policy(script: &str, name: &str) -> Result<String, PackagePolicyError> {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(dir.path(), "apt-cache", script);
        Apt::new()
            .with_apt_cache(program)
            .package_policy(name, &CancellationToken::new())
            .await
    }

    #[tokio::test]
    async fn package_policy_runs_in_the_c_locale_and_returns_its_text() {
        let _spawns = SPAWNS.lock().await;
        let script = r#"[ "$*" = 'policy -- python3.12' ] || exit 9
[ "$LC_ALL" = C ] || exit 8
printf 'python3.12:\n  Installed: 3.12.1-1+jammy1\n *** 3.12.1-1+jammy1 100\n'"#;
        assert_eq!(
            policy(script, "python3.12").await.expect("policy"),
            "python3.12:\n  Installed: 3.12.1-1+jammy1\n *** 3.12.1-1+jammy1 100\n"
        );
    }

    #[tokio::test]
    async fn an_unknown_package_s_policy_is_empty() {
        let _spawns = SPAWNS.lock().await;
        let script = "echo 'N: Unable to locate package nope' >&2";
        assert_eq!(policy(script, "nope").await.expect("policy"), "");
    }

    #[tokio::test]
    async fn a_failed_policy_is_the_policy_error() {
        let _spawns = SPAWNS.lock().await;
        let error = policy("echo 'E: broken cache' >&2; exit 100", "python3.12")
            .await
            .expect_err("failed");
        let text = error.to_string();
        assert!(
            text.starts_with("query package policy python3.12: "),
            "{text}"
        );
        assert!(
            text.contains("exited with code 100") && text.contains("E: broken cache"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_hung_policy_times_out() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut apt = Apt::new().with_apt_cache(stand_in(dir.path(), "apt-cache", "exec sleep 60"));
        apt.query_timeout = Duration::from_millis(200);
        let error = apt
            .package_policy("x", &CancellationToken::new())
            .await
            .expect_err("timed out");
        let text = error.to_string();
        assert!(
            text.starts_with("query package policy x: ") && text.contains("timed out"),
            "{text}"
        );
    }

    #[test]
    fn the_update_times_are_sb_gos() {
        assert_eq!(UPDATE_TIMING.attempt, Duration::from_secs(120));
        assert_eq!(UPDATE_TIMING.delay, Duration::from_secs(5));
        assert_eq!(UPDATE_TIMING.lock_retry, Duration::from_secs(5));
        assert_eq!(UPDATE_TIMING.lock_budget, Duration::from_secs(240));
    }

    #[test]
    fn durations_read_as_go_prints_them() {
        assert_eq!(go_duration(Duration::from_secs(120)), "2m0s");
        assert_eq!(go_duration(Duration::from_secs(5)), "5s");
        assert_eq!(go_duration(Duration::from_secs(10)), "10s");
        assert_eq!(go_duration(Duration::from_secs(65)), "1m5s");
        assert_eq!(go_duration(Duration::from_millis(200)), "200ms");
    }

    #[test]
    fn each_failure_has_its_next_step() {
        let mirror = |text: &str| next(Ending::Exited(100), text);
        assert_eq!(
            mirror("E: Failed to fetch x  Mirror sync in progress? [y]"),
            Next::MirrorError
        );
        assert_eq!(
            mirror("E: File has unexpected size (1 != 2)"),
            Next::MirrorError
        );
        assert_eq!(mirror("E: Hashes of expected file:"), Next::MirrorError);
        assert_eq!(
            mirror("E: Could not get lock /var/lib/apt/lists/lock. It is held by process 7"),
            Next::ListsLocked
        );
        assert_eq!(mirror("E: The repository x is not signed."), Next::Fail);
        assert_eq!(next(Ending::TimedOut, ""), Next::TimedOut);
        for ending in [Ending::Interrupted, Ending::Signalled(9), Ending::Exited(0)] {
            assert_eq!(
                next(ending, "Mirror sync in progress"),
                Next::Fail,
                "{ending:?}"
            );
        }
    }

    /// An apt-get in `dir` that logs its arguments to `calls.log` and
    /// `DEBIAN_FRONTEND` and `NEEDRESTART_MODE` to `env.log`, then runs
    /// `script`.
    fn apt_get(dir: &Path, script: &str) -> PathBuf {
        stand_in(
            dir,
            "apt-get",
            &format!(
                "printf '%s\\n' \"$*\" >> '{calls}'\n\
                 printf '%s %s\\n' \"$DEBIAN_FRONTEND\" \"$NEEDRESTART_MODE\" >> '{env}'\n\
                 {script}",
                calls = dir.join("calls.log").display(),
                env = dir.join("env.log").display(),
            ),
        )
    }

    fn lines(dir: &Path, name: &str) -> Vec<String> {
        std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// apt with `program` as apt-get and short update times: `attempt`,
    /// 10 ms and 20 ms delays, a 10 ms lists-lock retry within 30 ms.
    fn timed(program: PathBuf, attempt: Duration) -> Apt {
        Apt::new().with_apt_get(program).with_update_timing(
            attempt,
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(30),
        )
    }

    /// [`timed`] with 20 s attempts, so a slow stand-in on a loaded host
    /// never times out.
    fn quick(program: PathBuf) -> Apt {
        timed(program, Duration::from_secs(20))
    }

    #[tokio::test]
    async fn add_repositories_names_what_it_moved() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let (etc_apt, var_backups) = (dir.path().join("etc/apt"), dir.path().join("var/backups"));
        std::fs::create_dir_all(etc_apt.join("sources.list.d")).expect("mkdir");
        std::fs::create_dir_all(&var_backups).expect("mkdir");
        for path in [etc_apt.join("sources.list.d"), var_backups.clone()] {
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .expect("chmod");
        }
        std::fs::write(etc_apt.join("sources.list.d/docker.list"), "docker\n").expect("write");
        let lsb = stand_in(
            dir.path(),
            "lsb_release",
            "echo 'No LSB modules are available.' >&2; echo jammy",
        );
        let apt = Apt::new().with_lsb_release(lsb).with_sources_roots(
            &etc_apt,
            &var_backups,
            rustix::process::geteuid().as_raw(),
        );
        let ran = in_step(async |step, _| Ok(apt.add_repositories(step, "0a1b").await?)).await;
        assert_eq!(ran.error, None, "{}", ran.screen);
        let line = format!(
            "Moved 1 entries from {} to {}",
            etc_apt.join("sources.list.d").display(),
            var_backups.join("sb/apt-sources/0a1b").display()
        );
        assert!(ran.screen.contains(&line), "{}", ran.screen);
        assert!(
            std::fs::read_to_string(etc_apt.join("sources.list"))
                .expect("read")
                .contains(" jammy main")
        );
    }

    #[test]
    fn remove_sources_mentioning_returns_where_the_files_went() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (etc_apt, var_backups) = (dir.path().join("etc/apt"), dir.path().join("var/backups"));
        std::fs::create_dir_all(etc_apt.join("sources.list.d")).expect("mkdir");
        std::fs::create_dir_all(&var_backups).expect("mkdir");
        for path in [etc_apt.join("sources.list.d"), var_backups.clone()] {
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .expect("chmod");
        }
        let sources_d = etc_apt.join("sources.list.d");
        std::fs::write(sources_d.join("ppa.list"), "deadsnakes/ppa\n").expect("write");
        std::fs::write(sources_d.join("docker.list"), "docker\n").expect("write");
        let apt = Apt::new().with_sources_roots(
            &etc_apt,
            &var_backups,
            rustix::process::geteuid().as_raw(),
        );
        let moved = apt
            .remove_sources_mentioning("deadsnakes", "deadsnakes", "0a1b")
            .expect("remove");
        assert_eq!(moved, [var_backups.join("sb/deadsnakes/0a1b/ppa.list")]);
        assert!(sources_d.join("docker.list").exists());
    }

    #[test]
    fn sources_mentioning_names_what_is_still_there() {
        let dir = tempfile::tempdir().expect("tempdir");
        let etc_apt = dir.path().join("etc/apt");
        let sources_d = etc_apt.join("sources.list.d");
        std::fs::create_dir_all(&sources_d).expect("mkdir");
        std::fs::set_permissions(
            &sources_d,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod");
        std::fs::write(sources_d.join("ppa.list"), "deadsnakes/ppa\n").expect("write");
        std::fs::write(sources_d.join("docker.list"), "docker\n").expect("write");
        let apt = Apt::new().with_sources_roots(
            &etc_apt,
            dir.path().join("var/backups"),
            rustix::process::geteuid().as_raw(),
        );
        assert_eq!(
            apt.sources_mentioning("deadsnakes").expect("query"),
            [sources_d.join("ppa.list")]
        );
        assert!(sources_d.join("ppa.list").exists());
    }

    #[tokio::test]
    async fn a_failed_codename_is_sb_gos_error() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let lsb = stand_in(dir.path(), "lsb_release", "echo 'broken' >&2; exit 3");
        let apt = Apt::new()
            .with_lsb_release(lsb)
            .with_sources_roots(dir.path(), dir.path(), 0);
        let ran = in_step(async |step, _| Ok(apt.add_repositories(step, "0a1b").await?)).await;
        let error = ran.error.expect("failed");
        assert!(
            error.starts_with("error getting Ubuntu release codename: "),
            "{error}"
        );
        assert!(error.contains("exited with code 3"), "{error}");
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("list").count(),
            1,
            "only the stand-in"
        );
    }

    #[tokio::test]
    async fn the_roots_owner_is_the_one_checked() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let sources_d = dir.path().join("sources.list.d");
        std::fs::create_dir(&sources_d).expect("mkdir");
        std::fs::set_permissions(
            &sources_d,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod");
        let lsb = stand_in(dir.path(), "lsb_release", "echo jammy");
        let other = rustix::process::geteuid().as_raw() + 1;
        let apt =
            Apt::new()
                .with_lsb_release(lsb)
                .with_sources_roots(dir.path(), dir.path(), other);
        let ran = in_step(async |step, _| Ok(apt.add_repositories(step, "0a1b").await?)).await;
        assert_eq!(
            ran.error.expect("refused"),
            format!(
                "refusing to clean {}: not owned by root",
                sources_d.display()
            )
        );
    }

    #[tokio::test]
    async fn a_hung_codename_times_out() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let lsb = stand_in(dir.path(), "lsb_release", "exec sleep 60");
        let mut apt =
            Apt::new()
                .with_lsb_release(lsb)
                .with_sources_roots(dir.path(), dir.path(), 0);
        apt.codename_timeout = Duration::from_millis(200);
        let ran = in_step(async |step, _| Ok(apt.add_repositories(step, "0a1b").await?)).await;
        let error = ran.error.expect("timed out");
        assert!(
            error.starts_with("error getting Ubuntu release codename: ")
                && error.contains("timed out"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn what_moved_is_reported_when_a_later_move_fails() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let (etc_apt, var_backups) = (dir.path().join("etc/apt"), dir.path().join("var/backups"));
        std::fs::create_dir_all(etc_apt.join("sources.list.d")).expect("mkdir");
        std::fs::create_dir_all(&var_backups).expect("mkdir");
        for path in [etc_apt.join("sources.list.d"), var_backups.clone()] {
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .expect("chmod");
        }
        for name in ["a.list", "b.list"] {
            std::fs::write(etc_apt.join("sources.list.d").join(name), "x\n").expect("write");
        }
        let lsb = stand_in(dir.path(), "lsb_release", "echo jammy");
        let mut apt = Apt::new().with_lsb_release(lsb).with_sources_roots(
            &etc_apt,
            &var_backups,
            rustix::process::geteuid().as_raw(),
        );
        apt.sources.rename = |from, name, to, new| {
            if name == c"b.list" {
                Err(rustix::io::Errno::PERM)
            } else {
                rustix::fs::renameat(from, name, to, new)
            }
        };
        let ran = in_step(async |step, _| Ok(apt.add_repositories(step, "0a1b").await?)).await;
        assert!(ran.error.expect("failed").starts_with("error moving "));
        let line = format!(
            "Moved 1 entries from {} to {}",
            etc_apt.join("sources.list.d").display(),
            var_backups.join("sb/apt-sources/0a1b").display()
        );
        assert!(ran.screen.contains(&line), "{}", ran.screen);
    }

    #[test]
    fn the_codename_limit_is_five_seconds() {
        assert_eq!(CODENAME_TIMEOUT, Duration::from_secs(5));
    }

    const POLICY: &str = "-o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold";

    #[tokio::test]
    async fn install_runs_sb_gos_policy_with_apt_s_lock_wait() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = quick(apt_get(dir.path(), "exit 0"));
        let ran = in_step(async |step, _| apt.install(step, &["libpq-dev", "git"]).await).await;
        assert_eq!(ran.error, None, "{}", ran.screen);
        assert_eq!(
            lines(dir.path(), "calls.log"),
            [format!(
                "{POLICY} -o DPkg::Lock::Timeout=240 install -y libpq-dev git"
            )]
        );
        assert_eq!(lines(dir.path(), "env.log"), ["noninteractive l"]);
    }

    #[tokio::test]
    async fn remove_and_autoremove_wait_for_the_lock_too() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = quick(apt_get(dir.path(), "exit 0"));
        let ran = in_step(async |step, _| {
            apt.remove(step, &["python3.12"]).await?;
            apt.autoremove(step).await
        })
        .await;
        assert_eq!(ran.error, None, "{}", ran.screen);
        assert_eq!(
            lines(dir.path(), "calls.log"),
            [
                format!("{POLICY} -o DPkg::Lock::Timeout=240 remove -y python3.12"),
                format!("{POLICY} -o DPkg::Lock::Timeout=240 autoremove -y"),
            ]
        );
        assert_eq!(lines(dir.path(), "env.log"), ["noninteractive l"; 2]);
    }

    #[tokio::test]
    async fn a_failed_install_is_sb_gos_error_over_exec_s() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = quick(apt_get(
            dir.path(),
            "echo 'E: Unable to locate package nope' >&2; exit 23",
        ));
        let ran = in_step(async |step, _| apt.install(step, &["git", "nope"]).await).await;
        let error = ran.error.expect("failed");
        assert!(
            error.starts_with("failed to install packages 'git, nope': "),
            "{error}"
        );
        assert!(error.contains("exited with code 23"), "{error}");
        assert!(
            ran.screen.contains("E: Unable to locate package nope"),
            "{}",
            ran.screen
        );
    }

    #[tokio::test]
    async fn update_has_no_lock_timeout_and_succeeds_at_once() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = quick(apt_get(dir.path(), "exit 0"));
        let ran = in_step(async |step, _| apt.update_lists(step).await).await;
        assert_eq!(ran.error, None, "{}", ran.screen);
        assert_eq!(lines(dir.path(), "calls.log"), [format!("{POLICY} update")]);
        assert_eq!(lines(dir.path(), "env.log"), ["noninteractive l"]);
    }

    /// A script that fails its first `failures` runs with `stderr` (exit
    /// 100), counting runs in `count`, and exits 0 after.
    fn failing(dir: &Path, failures: u32, stderr: &str) -> String {
        format!(
            "n=$(cat '{count}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{count}'\n\
             if [ $n -le {failures} ]; then echo '{stderr}' >&2; exit 100; fi\n\
             exit 0",
            count = dir.join("count").display(),
        )
    }

    const MIRROR: &str =
        "E: Failed to fetch x  File has unexpected size (1 != 2). Mirror sync in progress? [y]";
    const LOCKED: &str =
        "E: Could not get lock /var/lib/apt/lists/lock. It is held by process 7 (apt-get)";

    #[tokio::test]
    async fn a_mirror_error_twice_is_retried_with_sb_gos_lines() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = quick(apt_get(dir.path(), &failing(dir.path(), 2, MIRROR)));
        let ran = in_step(async |step, _| apt.update_lists(step).await).await;
        assert_eq!(ran.error, None, "{}", ran.screen);
        assert_eq!(lines(dir.path(), "calls.log").len(), 3);
        for line in [
            "apt-get update failed (attempt 1/3), retrying in 10ms due to transient mirror sync error...",
            "apt-get update failed (attempt 2/3), retrying in 20ms due to transient mirror sync error...",
        ] {
            assert!(ran.screen.contains(line), "{line}: {}", ran.screen);
        }
    }

    #[tokio::test]
    async fn a_mirror_error_three_times_fails() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = quick(apt_get(dir.path(), &failing(dir.path(), 9, MIRROR)));
        let ran = in_step(async |step, _| apt.update_lists(step).await).await;
        let error = ran.error.expect("failed");
        assert!(
            error.starts_with("failed to update package lists: "),
            "{error}"
        );
        assert!(error.contains("exited with code 100"), "{error}");
        assert_eq!(lines(dir.path(), "calls.log").len(), 3);
    }

    #[tokio::test]
    async fn another_error_is_not_retried() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let script = failing(dir.path(), 9, "E: The repository x is not signed.");
        let apt = quick(apt_get(dir.path(), &script));
        let ran = in_step(async |step, _| apt.update_lists(step).await).await;
        assert!(ran.error.is_some());
        assert_eq!(lines(dir.path(), "calls.log").len(), 1);
    }

    #[tokio::test]
    async fn a_hung_update_times_out_three_times() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = timed(
            apt_get(dir.path(), "exec sleep 30"),
            Duration::from_millis(200),
        );
        let ran = in_step(async |step, _| apt.update_lists(step).await).await;
        let error = ran.error.expect("failed");
        assert!(
            error.starts_with("failed to update package lists: "),
            "{error}"
        );
        assert!(error.contains("timed out"), "{error}");
        assert_eq!(lines(dir.path(), "calls.log").len(), 3);
        assert!(
            ran.screen.contains(
                "apt-get update timed out after 200ms (attempt 1/3), retrying in 10ms..."
            ),
            "{}",
            ran.screen
        );
    }

    #[tokio::test]
    async fn a_locked_lists_waits_outside_the_attempts() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        // Locked twice, then mirror errors from the third run on: two
        // lock waits, then the three attempts.
        let script = format!(
            "n=$(cat '{count}' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{count}'\n\
             if [ $n -le 2 ]; then echo '{LOCKED}' >&2; exit 100; fi\n\
             echo '{MIRROR}' >&2; exit 100",
            count = dir.path().join("count").display(),
        );
        let apt = quick(apt_get(dir.path(), &script));
        let ran = in_step(async |step, _| apt.update_lists(step).await).await;
        assert!(ran.error.is_some());
        assert_eq!(lines(dir.path(), "calls.log").len(), 5);
    }

    #[tokio::test]
    async fn a_lock_held_past_the_budget_fails() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = quick(apt_get(dir.path(), &failing(dir.path(), 99, LOCKED)));
        let ran = in_step(async |step, _| apt.update_lists(step).await).await;
        let error = ran.error.expect("failed");
        assert!(
            error.starts_with("failed to update package lists: "),
            "{error}"
        );
        // 30 ms of 10 ms waits: three more tries after the first.
        assert_eq!(lines(dir.path(), "calls.log").len(), 4);
    }

    /// SIGINT's number, as the runner's signal handler passes it.
    const SIGINT: i32 = 2;

    #[tokio::test]
    async fn an_interrupted_attempt_is_never_retried() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = Apt::new()
            .with_apt_get(apt_get(dir.path(), "exec sleep 30"))
            .with_update_timing(
                Duration::from_secs(20),
                Duration::from_millis(10),
                Duration::from_millis(10),
                Duration::from_millis(30),
            );
        let ran = in_step(async |step, shutdown| {
            let (result, ()) = tokio::join!(apt.update_lists(step), async {
                tokio::time::sleep(Duration::from_millis(300)).await;
                shutdown.signalled(SIGINT);
            });
            result
        })
        .await;
        let error = ran.error.expect("failed");
        assert!(error.contains("was interrupted"), "{error}");
        assert_eq!(lines(dir.path(), "calls.log").len(), 1);
    }

    #[tokio::test]
    async fn ctrl_c_during_the_delay_is_sb_gos_cancelled_error() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let apt = Apt::new()
            .with_apt_get(apt_get(dir.path(), &failing(dir.path(), 9, MIRROR)))
            .with_update_timing(
                Duration::from_secs(20),
                Duration::from_secs(20),
                Duration::from_millis(10),
                Duration::from_millis(30),
            );
        let count = dir.path().join("count");
        let ran = in_step(async |step, shutdown| {
            let (result, ()) = tokio::join!(apt.update_lists(step), async {
                // After the first attempt has failed: the delay is 20 s.
                while !count.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                shutdown.signalled(SIGINT);
            });
            result
        })
        .await;
        assert_eq!(
            ran.error.as_deref(),
            Some("failed to update package lists: context cancelled during retry: interrupted")
        );
        assert_eq!(lines(dir.path(), "calls.log").len(), 1);
    }
}
