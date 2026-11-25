//! The setup checks (host spec, "Setup checks"; sb-go's host/system.go):
//! the CPU architecture, LXC and the desktop, each refusing with sb-go's
//! text. ansible's `install` and ops' setup run them.

use std::path::PathBuf;
use std::time::Duration;

use sb_exec::{Cmd, Ending};
use tokio_util::sync::CancellationToken;

use crate::apt::{Apt, PackageQueryError};

/// sb-go's limit on `systemd-detect-virt` and on the desktop query.
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The package whose installed state refuses setup.
const DESKTOP: &str = "ubuntu-desktop";

/// Why setup can't go on here, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    /// The CPU isn't `x86_64`; the machine `uname(2)` names.
    #[error(
        "UNSUPPORTED CPU Architecture - Install cancelled: {0} is not supported. Supported: x86_64"
    )]
    Architecture(String),
    /// sb runs in an LXC container.
    #[error(
        "UNSUPPORTED VIRTUALIZATION - Install cancelled: Running in an LXC container is not supported"
    )]
    Lxc,
    /// systemd-detect-virt failed: how, and what it printed.
    #[error("could not detect virtualization using systemd-detect-virt: {cause}, output: {output}")]
    DetectVirt { cause: String, output: String },
    /// `ubuntu-desktop` is installed.
    #[error("UNSUPPORTED DESKTOP INSTALL - Install cancelled: Only Ubuntu Server is supported")]
    Desktop,
    /// Its installed state couldn't be read.
    #[error("unexpected error checking for desktop environment: {0}")]
    DesktopQuery(PackageQueryError),
}

/// The variable a test build of sb reads its machine from instead of
/// `uname(2)` ([`Setup::for_sb`]): sb's end-to-end tests run on any CPU,
/// and setup refuses all but `x86_64`.
#[cfg(feature = "testing")]
pub const MACHINE_VAR: &str = "SB_TEST_MACHINE";

/// The setup checks, with the programs and the machine they read.
#[derive(Debug, Clone)]
pub struct Setup {
    detect_virt: PathBuf,
    apt: Apt,
    timeout: Duration,
    machine: fn() -> String,
}

impl Default for Setup {
    fn default() -> Self {
        Self::new()
    }
}

impl Setup {
    /// The programs from `PATH`, this host's machine.
    #[must_use]
    pub fn new() -> Self {
        Self {
            detect_virt: PathBuf::from("systemd-detect-virt"),
            apt: Apt::new(),
            timeout: CHECK_TIMEOUT,
            machine,
        }
    }

    /// The checks `sb` runs with: [`Setup::new`]; in a build with the
    /// `testing` feature (the sb `cargo test` runs), the machine
    /// [`MACHINE_VAR`] names when it is set, as python-env's `for_sb`
    /// takes its stand-in tree.
    #[must_use]
    pub fn for_sb() -> Self {
        #[cfg(feature = "testing")]
        if std::env::var_os(MACHINE_VAR).is_some() {
            return Self::new().with_machine(|| std::env::var(MACHINE_VAR).unwrap_or_default());
        }
        Self::new()
    }

    /// Run `program` as systemd-detect-virt instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_detect_virt(mut self, program: impl Into<PathBuf>) -> Self {
        self.detect_virt = program.into();
        self
    }

    /// Query packages with `apt` instead (tests).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_apt(mut self, apt: Apt) -> Self {
        self.apt = apt;
        self
    }

    /// Read the machine from `machine` instead of `uname(2)` (tests;
    /// M-MOCKABLE-SYSCALLS).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_machine(mut self, machine: fn() -> String) -> Self {
        self.machine = machine;
        self
    }

    /// Refuse any CPU but `x86_64` (sb-go's `CheckArchitecture`), from
    /// `uname(2)`'s machine rather than a `uname -m` child (host spec,
    /// difference 4).
    ///
    /// # Errors
    ///
    /// [`SetupError::Architecture`] naming the machine.
    pub fn check_architecture(&self) -> Result<(), SetupError> {
        let machine = (self.machine)();
        // sb-go's `(x86_64)$`.
        if machine.ends_with("x86_64") {
            Ok(())
        } else {
            Err(SetupError::Architecture(machine))
        }
    }

    /// Refuse an LXC container (sb-go's `CheckLXC`): `systemd-detect-virt
    /// -c` within 5 s, its stdout and stderr read together.
    ///
    /// - Exit 0 naming any container but `lxc` (lowercased) passes.
    /// - A non-zero exit passes only when the output is `none`, which
    ///   is how systemd-detect-virt says "no container" (exit 1).
    ///
    /// # Errors
    ///
    /// [`SetupError::Lxc`] in LXC; [`SetupError::DetectVirt`] when the
    /// program can't run, times out, is interrupted or fails with other
    /// output.
    pub async fn check_lxc(&self, token: &CancellationToken) -> Result<(), SetupError> {
        let ran = Cmd::new(self.detect_virt.as_os_str())
            .arg("-c")
            .timeout(self.timeout)
            .capture(token)
            .await
            .map_err(|err| SetupError::DetectVirt {
                cause: format!("{err}: {}", err.cause()),
                output: String::new(),
            })?;
        let output = format!("{}{}", ran.stdout, ran.stderr);
        let output = output.trim();
        match ran.outcome.ending {
            Ending::Exited(0) if output.to_lowercase() == "lxc" => Err(SetupError::Lxc),
            Ending::Exited(0) => Ok(()),
            Ending::Exited(_) if output == "none" => Ok(()),
            ending => Err(SetupError::DetectVirt {
                cause: ending.to_string(),
                // On one line, controls escaped: std's `str::escape_debug`
                // (<https://doc.rust-lang.org/std/primitive.str.html#method.escape_debug>).
                output: output.escape_debug().to_string(),
            }),
        }
    }

    /// Refuse a host with `ubuntu-desktop` installed (sb-go's
    /// `CheckDesktopEnvironment`), by its installed state within sb-go's
    /// 5 s: a package dpkg only remembers (`rc`, `un`) doesn't count (host
    /// spec, difference 2).
    ///
    /// # Errors
    ///
    /// [`SetupError::Desktop`] when it's installed;
    /// [`SetupError::DesktopQuery`] when dpkg-query fails.
    pub async fn check_desktop(&self, token: &CancellationToken) -> Result<(), SetupError> {
        match self
            .apt
            .installed_within(DESKTOP, self.timeout, token)
            .await
        {
            Ok(false) => Ok(()),
            Ok(true) => Err(SetupError::Desktop),
            Err(err) => Err(SetupError::DesktopQuery(err)),
        }
    }
}

/// This host's machine: rustix's `uname(2)`
/// (<https://docs.rs/rustix/1.1.6/rustix/system/fn.uname.html>; host spec,
/// assumption 3). Why not sysinfo's `System::cpu_arch`, which sb-host
/// already links: it falls back to the build's own architecture when the
/// read fails, which would pass an `x86_64` build on any CPU.
fn machine() -> String {
    rustix::system::uname()
        .machine()
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{SPAWNS, stand_in};

    fn on(machine: fn() -> String) -> Result<(), SetupError> {
        Setup::new().with_machine(machine).check_architecture()
    }

    #[test]
    fn x86_64_passes_and_anything_else_is_refused() {
        assert!(on(|| "x86_64".into()).is_ok());
        let error = on(|| "aarch64".into()).expect_err("refused");
        assert_eq!(
            error.to_string(),
            "UNSUPPORTED CPU Architecture - Install cancelled: aarch64 is not supported. Supported: x86_64"
        );
        assert!(on(|| "x86_64 ".into()).is_err());
        assert!(on(|| "i686".into()).is_err());
    }

    #[test]
    fn the_system_machine_is_read() {
        let machine = machine();
        // uname's machine is the architecture Rust names for the
        // supported targets (x86_64, aarch64), so the test binary's own.
        assert_eq!(machine, std::env::consts::ARCH);
    }

    async fn lxc(script: &str) -> Result<(), SetupError> {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(
            dir.path(),
            "systemd-detect-virt",
            &format!("[ \"$*\" = '-c' ] || exit 9\n{script}"),
        );
        Setup::new()
            .with_detect_virt(program)
            .check_lxc(&CancellationToken::new())
            .await
    }

    #[tokio::test]
    async fn no_container_passes() {
        let _spawns = SPAWNS.lock().await;
        assert!(lxc("echo none; exit 1").await.is_ok());
    }

    #[tokio::test]
    async fn another_container_passes() {
        let _spawns = SPAWNS.lock().await;
        assert!(lxc("echo docker").await.is_ok());
    }

    #[tokio::test]
    async fn lxc_is_refused_in_any_case() {
        let _spawns = SPAWNS.lock().await;
        for answer in ["lxc", "LXC"] {
            let error = lxc(&format!("echo {answer}")).await.expect_err(answer);
            assert_eq!(
                error.to_string(),
                "UNSUPPORTED VIRTUALIZATION - Install cancelled: Running in an LXC container is not supported"
            );
        }
    }

    #[tokio::test]
    async fn none_on_stderr_counts_as_output() {
        let _spawns = SPAWNS.lock().await;
        assert!(lxc("echo none >&2; exit 1").await.is_ok());
    }

    #[tokio::test]
    async fn lxc_on_stderr_counts_as_output() {
        let _spawns = SPAWNS.lock().await;
        assert!(matches!(lxc("echo lxc >&2").await, Err(SetupError::Lxc)));
    }

    #[tokio::test]
    async fn a_failure_with_other_output_names_both() {
        let _spawns = SPAWNS.lock().await;
        let error = lxc("echo 'Failed to check'; exit 2")
            .await
            .expect_err("failed");
        assert_eq!(
            error.to_string(),
            "could not detect virtualization using systemd-detect-virt: exited with code 2, output: Failed to check"
        );
    }

    #[tokio::test]
    async fn exit_0_with_none_passes() {
        let _spawns = SPAWNS.lock().await;
        assert!(lxc("echo none").await.is_ok());
    }

    #[tokio::test]
    async fn a_hung_check_times_out() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(
            dir.path(),
            "systemd-detect-virt",
            "echo none; exec sleep 60",
        );
        let mut setup = Setup::new().with_detect_virt(program);
        setup.timeout = Duration::from_millis(200);
        let error = setup
            .check_lxc(&CancellationToken::new())
            .await
            .expect_err("timed out");
        assert_eq!(
            error.to_string(),
            "could not detect virtualization using systemd-detect-virt: timed out, output: none"
        );
    }

    #[tokio::test]
    async fn a_missing_program_is_the_detect_error() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let error = Setup::new()
            .with_detect_virt(dir.path().join("missing"))
            .check_lxc(&CancellationToken::new())
            .await
            .expect_err("failed");
        let text = error.to_string();
        assert!(
            text.starts_with(
                "could not detect virtualization using systemd-detect-virt: could not run "
            ),
            "{text}"
        );
        assert!(text.ends_with(", output: "), "{text}");
    }

    async fn desktop(script: &str) -> Result<(), SetupError> {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(
            dir.path(),
            "dpkg-query",
            &format!("[ \"$3\" = 'ubuntu-desktop' ] || exit 9\n{script}"),
        );
        Setup::new()
            .with_apt(Apt::new().with_dpkg_query(program))
            .check_desktop(&CancellationToken::new())
            .await
    }

    #[tokio::test]
    async fn an_installed_desktop_is_refused() {
        let _spawns = SPAWNS.lock().await;
        let error = desktop("printf 'ii '").await.expect_err("refused");
        assert_eq!(
            error.to_string(),
            "UNSUPPORTED DESKTOP INSTALL - Install cancelled: Only Ubuntu Server is supported"
        );
    }

    #[tokio::test]
    async fn a_removed_or_unknown_desktop_passes() {
        let _spawns = SPAWNS.lock().await;
        assert!(desktop("printf 'rc '").await.is_ok());
        assert!(desktop("exit 1").await.is_ok());
    }

    #[tokio::test]
    async fn a_failed_desktop_query_nests() {
        let _spawns = SPAWNS.lock().await;
        let error = desktop("exit 2").await.expect_err("failed");
        let text = error.to_string();
        assert!(
            text.starts_with(
                "unexpected error checking for desktop environment: query package ubuntu-desktop: "
            ),
            "{text}"
        );
    }

    #[test]
    fn the_checks_keep_sb_gos_five_seconds() {
        assert_eq!(Setup::new().timeout, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn the_desktop_query_takes_the_checks_limit() {
        let _spawns = SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let program = stand_in(dir.path(), "dpkg-query", "exec sleep 60");
        let mut setup = Setup::new().with_apt(Apt::new().with_dpkg_query(program));
        setup.timeout = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let error = setup
            .check_desktop(&CancellationToken::new())
            .await
            .expect_err("timed out");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        assert!(error.to_string().contains("timed out"), "{error}");
    }
}
