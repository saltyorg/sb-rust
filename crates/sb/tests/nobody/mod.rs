//! sb as `nobody`, for the tests of what an unprivileged run does
//! (cli-core decision 3): `setpriv` to uid and gid 65534, with a stand-in
//! `sudo` first on `PATH` that the unprivileged user can run. Switching
//! users needs root, so callers run only with `SB_TEST_ROOT=1`
//! (`root_gate`). A module of its own, so test binaries that don't switch
//! users don't carry it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Run sb as `nobody` with `sudo_script` as its `sudo`, logging to
/// `log_dir` if it logged at all.
///
/// `sudo_dir` holds the script (made world-readable, so `nobody` can run
/// it) and, in its `program` directory, the copy of sb that runs. The
/// build under `target/` is only as reachable as the checkout: in a home
/// directory others can't enter (`/home/runner`, 0750, on GitHub's
/// runners) `nobody` can't run it at all. Returns sb's process ID (the
/// same through setpriv's and env's execs), its output and the copy's
/// path. The run gets
/// `common::sb()`'s environment (setpriv and env pass it on), with its
/// log, theme, Saltbox and Docker paths moved under `log_dir`, so it never
/// reaches the host's; `env` adds variables for this run. Only
/// called when the root tests were asked for, so a switch that fails is a
/// failure.
pub fn sb_as_nobody(
    args: &[&str],
    log_dir: &Path,
    sudo_dir: &Path,
    sudo_script: &str,
    env: &[(&str, &str)],
) -> (u32, Output, PathBuf) {
    sb_as_nobody_from(&sb_build(), args, log_dir, sudo_dir, sudo_script, env)
}

/// The sb build the tests run (`common::sb()`'s program).
pub fn sb_build() -> PathBuf {
    PathBuf::from(super::common::sb().get_program())
}

/// [`sb_as_nobody`] with the copy made from `build`, wherever it is.
pub fn sb_as_nobody_from(
    build: &Path,
    args: &[&str],
    log_dir: &Path,
    sudo_dir: &Path,
    sudo_script: &str,
    env: &[(&str, &str)],
) -> (u32, Output, PathBuf) {
    std::fs::set_permissions(sudo_dir, std::fs::Permissions::from_mode(0o755)).expect("chmod dir");
    let sudo = sudo_dir.join("sudo");
    sb_cli_core::testing::write_program(&sudo, sudo_script);
    let program_dir = sudo_dir.join("program");
    // A caller may run more than once with the same `sudo_dir`.
    std::fs::create_dir_all(&program_dir).expect("program dir");
    std::fs::set_permissions(&program_dir, std::fs::Permissions::from_mode(0o755))
        .expect("chmod program dir");
    let program = program_dir.join("sb");
    sb_cli_core::testing::copy_program(build, &program);
    // `nobody` must be able to run the stand-in, and the copy: a directory
    // it can't enter would make the PATH search skip the stand-in and find
    // the real sudo.
    for path in [&sudo, &program] {
        assert!(can_run(path), "nobody can't run {}", path.display());
    }
    let mut setpriv = as_nobody();
    super::common::apply_sb_env(&mut setpriv);
    let child = setpriv
        .arg("env")
        // Hermetic: sb sets no relaunch marker, so none may come in.
        .args(["-u", "SB_RELAUNCHED"])
        .arg(format!("PATH={}:/usr/bin:/bin", sudo_dir.display()))
        .arg(format!("SB_LOG_DIR={}", log_dir.display()))
        // The helper's own paths sit in a directory root made 0700, which
        // nobody can't look into (an unreadable theme file warns); these
        // are missing, not unreadable.
        .arg(format!(
            "SB_CONFIG={}",
            log_dir.join("no-such-config.toml").display()
        ))
        .arg(format!(
            "SB_SALTBOX_DIR={}",
            log_dir.join("no-saltbox").display()
        ))
        .arg(format!(
            "DOCKER_HOST=unix://{}",
            log_dir.join("no-docker.sock").display()
        ))
        .args(env.iter().map(|(name, value)| format!("{name}={value}")))
        .arg(&program)
        .args(args)
        // The test's own working directory is in the checkout, which
        // `nobody` may not be able to enter either.
        .current_dir(&program_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run setpriv");
    let pid = child.id();
    let output = child.wait_with_output().expect("wait for setpriv");
    // setpriv itself fails (not the fake's code, not ours) when it can't
    // switch users.
    assert!(
        !String::from_utf8_lossy(&output.stderr).starts_with("setpriv:"),
        "couldn't switch to nobody: {output:?}"
    );
    (pid, output, program)
}

/// `setpriv` switching to `nobody` (uid and gid 65534, no groups).
fn as_nobody() -> Command {
    let mut cmd = Command::new("setpriv");
    cmd.args(["--reuid=65534", "--regid=65534", "--clear-groups"]);
    cmd
}

/// Whether `nobody` can run `path`: asked as `nobody` (`test -x`), since
/// root's own access check answers for root.
pub fn can_run(path: &Path) -> bool {
    as_nobody()
        .args(["test", "-x"])
        .arg(path)
        .status()
        .expect("run setpriv")
        .success()
}
