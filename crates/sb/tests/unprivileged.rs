//! Spec success criterion 1: `sb version` and `sb --help` run as a normal
//! user with no sudo, and (decision 11) don't log.
//!
//! sb runs as `nobody` (setpriv) with a fake `sudo` first on `PATH` that
//! the unprivileged user can run and that exits 42; a wrong relaunch would
//! show as that code. Switching users needs root, so this runs only with
//! `SB_TEST_ROOT=1` (root_gate).

mod common;
mod root_gate;

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

/// Run sb as `nobody`, logging to `log_dir` if it logged at all. Only
/// called when the root tests were asked for, so a switch that fails is a
/// failure.
fn sb_as_nobody(args: &[&str], log_dir: &std::path::Path) -> Output {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod dir");
    let sudo = dir.path().join("sudo");
    std::fs::write(&sudo, "#!/bin/sh\necho 'sudo was called' >&2\nexit 42\n").expect("fake sudo");
    std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).expect("chmod sudo");
    let output = Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "env"])
        .arg(format!("PATH={}:/usr/bin:/bin", dir.path().display()))
        .arg(format!("SB_LOG_DIR={}", log_dir.display()))
        .arg(common::sb().get_program())
        .args(args)
        .output()
        .expect("run setpriv");
    // setpriv itself fails (not 42, not ours) when it can't switch users.
    assert!(
        !String::from_utf8_lossy(&output.stderr).starts_with("setpriv:"),
        "couldn't switch to nobody: {output:?}"
    );
    output
}

#[test]
fn version_and_help_never_call_sudo() {
    if !root_gate::requested() {
        return;
    }
    let parent = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let log_dir = parent.path().join("logs");
    for args in [&["version"][..], &["--help"][..]] {
        let out = sb_as_nobody(args, &log_dir);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "{args:?}");
    }
    // An unprivileged run doesn't log: the directory it could have made
    // (nobody may write the parent) doesn't exist.
    assert!(!log_dir.exists(), "an unprivileged run logged");
}
