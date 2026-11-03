//! sb as `nobody`, for the tests of what an unprivileged run does
//! (cli-core decision 3): `setpriv` to uid and gid 65534, with a stand-in
//! `sudo` first on `PATH` that the unprivileged user can run. Switching
//! users needs root, so callers run only with `SB_TEST_ROOT=1`
//! (root_gate). A module of its own, so test binaries that don't switch
//! users don't carry it.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

/// Run sb as `nobody` with `sudo_script` as its `sudo`, logging to
/// `log_dir` if it logged at all. `sudo_dir` holds the script (made
/// world-readable, so `nobody` can run it). Like `common::sb()`, the run
/// never reaches the host's Docker daemon: `DOCKER_HOST` names a socket
/// that doesn't exist and the developer's Docker settings are dropped.
/// Only called when the root tests were asked for, so a switch that fails
/// is a failure.
pub fn sb_as_nobody(args: &[&str], log_dir: &Path, sudo_dir: &Path, sudo_script: &str) -> Output {
    std::fs::set_permissions(sudo_dir, std::fs::Permissions::from_mode(0o755)).expect("chmod dir");
    let sudo = sudo_dir.join("sudo");
    std::fs::write(&sudo, sudo_script).expect("fake sudo");
    std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).expect("chmod sudo");
    let output = Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "env"])
        .args([
            "-u",
            "DOCKER_API_VERSION",
            "-u",
            "DOCKER_TLS_VERIFY",
            "-u",
            "DOCKER_CERT_PATH",
        ])
        .arg(format!("PATH={}:/usr/bin:/bin", sudo_dir.display()))
        .arg(format!("SB_LOG_DIR={}", log_dir.display()))
        .arg(format!(
            "DOCKER_HOST=unix://{}",
            log_dir.join("no-docker.sock").display()
        ))
        .arg(super::common::sb().get_program())
        .args(args)
        .output()
        .expect("run setpriv");
    // setpriv itself fails (not the fake's code, not ours) when it can't
    // switch users.
    assert!(
        !String::from_utf8_lossy(&output.stderr).starts_with("setpriv:"),
        "couldn't switch to nobody: {output:?}"
    );
    output
}
