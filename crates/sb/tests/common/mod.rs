//! Shared by the `sb` binary tests.

use std::path::PathBuf;
use std::sync::OnceLock;

/// A log directory for every test run of sb: tests run as root here, and a
/// root run logs; it must never reach the host's `/var/log/sb`.
pub fn test_log_dir() -> PathBuf {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("temp log dir"))
        .path()
        .to_path_buf()
}

/// The built `sb`, logging to [`test_log_dir`]. assert_cmd's prelude adds
/// `.assert()` to it.
pub fn sb() -> std::process::Command {
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("sb"));
    cmd.env("SB_LOG_DIR", test_log_dir());
    cmd
}
