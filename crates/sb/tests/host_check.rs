//! Spec success criterion 2: on an unsupported host every command,
//! `--help` included, exits 1 with sb-go's message.
//!
//! The host's `/etc/os-release` is never touched: each run happens in a
//! private mount namespace (`unshare --mount`) where a temporary file is
//! bind-mounted over it, seen only by that process and gone when it exits.
//! Creating the namespace needs root, so these run only with
//! `SB_TEST_ROOT=1` (root_gate).

mod common;
mod root_gate;

use std::io::Write;
use std::process::{Command, Output};

/// Run sb with `args` in a namespace whose `/etc/os-release` is `content`.
/// Only called when the root tests were asked for, so a namespace that
/// can't be made is a failure.
fn sb_with_os_release(content: &str, args: &[&str]) -> Output {
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

#[test]
fn an_unsupported_release_refuses_every_command() {
    if !root_gate::requested() {
        return;
    }
    for args in [&["version"][..], &["--help"][..]] {
        let out = sb_with_os_release("ID=ubuntu\nVERSION_ID=\"20.04\"\n", args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            String::from_utf8(out.stderr).expect("utf-8"),
            "✗ unsupported Ubuntu version (detected version: 20.04, supported versions: 22.04, 24.04, 26.04)\n",
            "{args:?}"
        );
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn another_distribution_is_refused() {
    if !root_gate::requested() {
        return;
    }
    let out = sb_with_os_release("ID=debian\nVERSION_ID=\"12\"\n", &["version"]);
    assert_eq!(out.status.code(), Some(1));
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
    assert_eq!(out.status.code(), Some(0));
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
        .arg(common::sb().get_program())
        .args(["--color=never", "docker", "ps"])
        .env("SB_COLOR_PROFILE", "16bit")
        .output()
        .expect("run unshare");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert_eq!(stderr.matches("SB_COLOR_PROFILE").count(), 1, "{stderr}");
    assert!(stderr.contains("unsupported Ubuntu version"), "{stderr}");
}
