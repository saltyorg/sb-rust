//! Spec success criterion 2: on an unsupported host every command,
//! `--help` included, exits 1 with sb-go's message.
//!
//! The host's `/etc/os-release` is never touched: each run happens in a
//! private mount namespace (`unshare --mount`) where a temporary file is
//! bind-mounted over it, seen only by that process and gone when it exits.
//! Creating the namespace needs root; elsewhere the test says it skipped.

mod common;

use std::io::Write;
use std::process::{Command, Output};

/// Run sb with `args` in a namespace whose `/etc/os-release` is `content`,
/// or `None` when no mount namespace can be created here.
fn sb_with_os_release(content: &str, args: &[&str]) -> Option<Output> {
    let probe = Command::new("unshare")
        .args(["--mount", "true"])
        .output()
        .ok()?;
    if !probe.status.success() {
        return None;
    }
    let mut fake = tempfile::NamedTempFile::new().expect("temp file");
    fake.write_all(content.as_bytes()).expect("write");
    let script = r#"mount --bind "$1" /etc/os-release && shift && exec "$@""#;
    let output = Command::new("unshare")
        .env("SB_LOG_DIR", common::test_log_dir())
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
        .arg(common::sb().get_program())
        .args(args)
        .output()
        .expect("run unshare");
    Some(output)
}

#[test]
fn an_unsupported_release_refuses_every_command() {
    for args in [&["version"][..], &["--help"][..]] {
        let Some(out) = sb_with_os_release("ID=ubuntu\nVERSION_ID=\"20.04\"\n", args) else {
            eprintln!("skipped: no mount namespace here (needs root)");
            return;
        };
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
    let Some(out) = sb_with_os_release("ID=debian\nVERSION_ID=\"12\"\n", &["version"]) else {
        eprintln!("skipped: no mount namespace here (needs root)");
        return;
    };
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stderr).expect("utf-8"),
        "✗ not an Ubuntu distribution (detected ID: debian)\n"
    );
}

#[test]
fn a_supported_release_runs() {
    let Some(out) = sb_with_os_release("ID=ubuntu\nVERSION_ID=\"26.04\"\n", &["version"]) else {
        eprintln!("skipped: no mount namespace here (needs root)");
        return;
    };
    assert_eq!(out.status.code(), Some(0));
}
