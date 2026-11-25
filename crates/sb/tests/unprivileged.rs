//! Spec success criterion 1: `sb version` and `sb --help` run as a normal
//! user with no sudo, and (decision 11) don't log.
//!
//! sb runs as `nobody` (setpriv) with a fake `sudo` first on `PATH` that
//! the unprivileged user can run and that exits 42; a wrong relaunch would
//! show as that code. Switching users needs root, so this runs only with
//! `SB_TEST_ROOT=1` (`root_gate`). The harness itself is checked here
//! too: it runs sb as `nobody` wherever the build is.

mod common;
mod nobody;
mod root_gate;

use std::os::unix::fs::PermissionsExt;

/// A stand-in sudo that exits 42: a wrong relaunch shows as that code.
const SUDO: &str = "#!/bin/sh\necho 'sudo was called' >&2\nexit 42\n";

#[test]
fn version_and_help_never_call_sudo() {
    if !root_gate::requested() {
        return;
    }
    let parent = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o777)).expect("chmod");
    let log_dir = parent.path().join("logs");
    for args in [&["version"][..], &["--help"][..]] {
        let sudo_dir = tempfile::tempdir().expect("temp dir");
        let (_, out, _) = nobody::sb_as_nobody(args, &log_dir, sudo_dir.path(), SUDO, &[]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.stdout.is_empty(), "{args:?}");
        // No startup warning: the test's theme and Saltbox paths are ones
        // nobody can look up (missing, not unreadable).
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("warning:"), "{args:?}: {stderr}");
    }
    // An unprivileged run doesn't log: the directory it could have made
    // (nobody may write the parent) doesn't exist.
    assert!(!log_dir.exists(), "an unprivileged run logged");
}

/// sb's build may sit where `nobody` can't reach it (a checkout under a
/// home directory others can't enter, as on GitHub's runners): the runs
/// as `nobody` still start, from a copy it can reach.
#[test]
fn a_build_nobody_cannot_reach_still_runs_as_nobody() {
    if !root_gate::requested() {
        return;
    }
    let locked = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(locked.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let build = locked.path().join("sb");
    sb_cli_core::testing::copy_program(&nobody::sb_build(), &build);
    assert!(
        !nobody::can_run(&build),
        "nobody can run {}",
        build.display()
    );
    let parent = tempfile::tempdir().expect("temp dir");
    let sudo_dir = tempfile::tempdir().expect("temp dir");
    let (_, out, _) = nobody::sb_as_nobody_from(
        &build,
        &["version"],
        &parent.path().join("logs"),
        sudo_dir.path(),
        SUDO,
        &[],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stdout.starts_with(b"Saltbox CLI version: "), "{out:?}");
}
