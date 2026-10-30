//! Spec success criterion 1: `sb version` and `sb --help` run as a normal
//! user with no sudo.
//!
//! sb runs as `nobody` (setpriv) with a fake `sudo` first on `PATH` that
//! the unprivileged user can run and that exits 42; a wrong relaunch would
//! show as that code. Switching users needs root; elsewhere the test says
//! it skipped.

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

fn sb_as_nobody(args: &[&str]) -> Option<Output> {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod dir");
    let sudo = dir.path().join("sudo");
    std::fs::write(&sudo, "#!/bin/sh\necho 'sudo was called' >&2\nexit 42\n").expect("fake sudo");
    std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).expect("chmod sudo");
    let output = Command::new("setpriv")
        .args(["--reuid=65534", "--regid=65534", "--clear-groups", "env"])
        .arg(format!("PATH={}:/usr/bin:/bin", dir.path().display()))
        .arg(assert_cmd::cargo::cargo_bin("sb"))
        .args(args)
        .output()
        .ok()?;
    // setpriv itself fails (not 42, not ours) when we can't switch users.
    let switched = !String::from_utf8_lossy(&output.stderr).starts_with("setpriv:");
    switched.then_some(output)
}

#[test]
fn version_and_help_never_call_sudo() {
    for args in [&["version"][..], &["--help"][..]] {
        let Some(out) = sb_as_nobody(args) else {
            eprintln!("skipped: can't switch to an unprivileged user here (needs root)");
            return;
        };
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "{args:?}");
    }
}
