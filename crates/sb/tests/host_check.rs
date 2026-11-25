//! Spec success criterion 2: on an unsupported host every command,
//! `--help` included, exits 1 with sb-go's message.
//!
//! The host's `/etc/os-release` is never touched: each run happens in a
//! private mount namespace (`unshare --mount`) where a temporary file is
//! bind-mounted over it, seen only by that process and gone when it exits.
//! Creating the namespace needs root, so these run only with
//! `SB_TEST_ROOT=1` (`root_gate`).

mod common;
mod root_gate;

use std::io::Write;
use std::process::{Command, Output};

/// Run sb with `args` in a namespace whose `/etc/os-release` is `content`.
/// Only called when the root tests were asked for, so a namespace that
/// can't be made is a failure.
fn sb_with_os_release(content: &str, args: &[&str]) -> Output {
    sb_with_os_release_env(content, args, &[])
}

/// [`sb_with_os_release`], with `env` set for sb.
fn sb_with_os_release_env(
    content: &str,
    args: &[&str],
    env: &[(&str, &std::path::Path)],
) -> Output {
    let probe = Command::new("unshare")
        .args(["--mount", "true"])
        .output()
        .expect("run unshare");
    assert!(probe.status.success(), "no mount namespace: {probe:?}");
    let mut fake = tempfile::NamedTempFile::new().expect("temp file");
    fake.write_all(content.as_bytes()).expect("write");
    let script = r#"mount --bind "$1" /etc/os-release && shift && exec "$@""#;
    let mut cmd = Command::new("unshare");
    common::apply_sb_env(&mut cmd);
    for (name, value) in env {
        cmd.env(name, value);
    }
    cmd.args([
        "--mount",
        "--propagation",
        "private",
        "sh",
        "-c",
        script,
        "sh",
    ])
    .arg(fake.path())
    .arg(common::sb().get_program())
    .args(args)
    .output()
    .expect("run unshare")
}

/// Every command but `version` and `self-update` (cli-core spec, startup
/// step 1): `--help` and `update` included.
#[test]
fn an_unsupported_release_refuses_every_other_command() {
    if !root_gate::requested() {
        return;
    }
    for args in [&["--help"][..], &["update"][..], &["motd"][..]] {
        let out = sb_with_os_release("ID=ubuntu\nVERSION_ID=\"20.04\"\n", args);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {out:?}");
        assert_eq!(
            String::from_utf8(out.stderr).expect("utf-8"),
            "✗ unsupported Ubuntu version (detected version: 20.04, supported versions: 22.04, 24.04, 26.04)\n",
            "{args:?}"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

/// `version` and the standalone `self-update` run on an unsupported Ubuntu
/// release, after a warning naming it (maintainer, 2026-11-18).
#[test]
fn version_and_self_update_run_on_an_unsupported_release() {
    if !root_gate::requested() {
        return;
    }
    let warning = "warning: Ubuntu 20.04 isn't a supported release (supported versions: 22.04, 24.04, 26.04); sb self-update may bring a version that supports it\n";
    let out = sb_with_os_release(
        "ID=ubuntu\nVERSION_ID=\"20.04\"\n",
        &["--color=never", "version"],
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(String::from_utf8(out.stderr).expect("utf-8"), warning);
    assert!(
        String::from_utf8(out.stdout)
            .expect("utf-8")
            .starts_with("Saltbox CLI version: ")
    );
    // A raw build: self-update is off, and says so after the warning. A
    // test build reads the fake GitHub's bases even so.
    let fake = sb_release::testing::Fake::start().expect("fake");
    let dir = tempfile::tempdir().expect("dir");
    let bases = dir.path().join("bases.json");
    fake.write_bases(&bases).expect("bases");
    let out = sb_with_os_release_env(
        "ID=ubuntu\nVERSION_ID=\"20.04\"\n",
        &["--color=never", "self-update"],
        &[(sb_release::testing::BASES_VARIABLE, &bases)],
    );
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{shown}");
    assert!(shown.starts_with(warning), "{shown}");
    assert!(
        shown.contains("Self-update is disabled in this build"),
        "{shown}"
    );
}

#[test]
fn another_distribution_is_refused() {
    if !root_gate::requested() {
        return;
    }
    let out = sb_with_os_release("ID=debian\nVERSION_ID=\"12\"\n", &["version"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stderr).expect("utf-8"),
        "✗ not an Ubuntu distribution (detected ID: debian)\n"
    );
}

#[test]
fn a_supported_release_runs() {
    if !root_gate::requested() {
        return;
    }
    let out = sb_with_os_release("ID=ubuntu\nVERSION_ID=\"26.04\"\n", &["version"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
}

/// A command that needs root, run as `nobody` on an unsupported host: it
/// never relaunches, so it prints its startup warnings itself before the
/// refusal.
#[test]
fn an_unsupported_host_still_shows_a_relaunching_runs_warnings() {
    if !root_gate::requested() {
        return;
    }
    let probe = Command::new("unshare")
        .args(["--mount", "true"])
        .output()
        .expect("run unshare");
    assert!(probe.status.success(), "no mount namespace: {probe:?}");
    let mut fake = tempfile::NamedTempFile::new().expect("temp file");
    fake.write_all(b"ID=ubuntu\nVERSION_ID=\"20.04\"\n")
        .expect("write");
    std::fs::set_permissions(
        fake.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .expect("chmod");
    // sb runs as `nobody` from a copy in a directory it can enter: the
    // build under `target/` is only as reachable as the checkout (as in
    // `nobody::sb_as_nobody`).
    let program_dir = tempfile::tempdir().expect("temp dir");
    std::fs::set_permissions(
        program_dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .expect("chmod");
    let program = program_dir.path().join("sb");
    sb_cli_core::testing::copy_program(common::sb().get_program().as_ref(), &program);
    let script = r#"mount --bind "$1" /etc/os-release && shift && exec "$@""#;
    let mut cmd = Command::new("unshare");
    common::apply_sb_env(&mut cmd);
    let out = cmd
        .args([
            "--mount",
            "--propagation",
            "private",
            "sh",
            "-c",
            script,
            "sh",
        ])
        .arg(fake.path())
        .args([
            "setpriv",
            "--reuid=65534",
            "--regid=65534",
            "--clear-groups",
        ])
        .arg(&program)
        .args(["--color=never", "docker", "ps"])
        .env("SB_COLOR_PROFILE", "16bit")
        .output()
        .expect("run unshare");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert_eq!(stderr.matches("SB_COLOR_PROFILE").count(), 1, "{stderr}");
    assert!(stderr.contains("unsupported Ubuntu version"), "{stderr}");
}
