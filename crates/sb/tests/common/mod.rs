//! Shared by the `sb` binary tests.

use std::path::PathBuf;

/// A log directory for every test run of sb: tests run as root here, and a
/// root run logs; it must never reach the host's `/var/log/sb`. It lives in
/// Cargo's temporary directory for integration tests (under `target/`), so
/// `cargo clean` removes it and no run leaves a folder in /tmp
/// (https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-crates).
pub fn test_log_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sb-test-logs")
}

/// The built `sb`, logging to [`test_log_dir`]. assert_cmd's prelude adds
/// `.assert()` to it.
pub fn sb() -> std::process::Command {
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("sb"));
    cmd.env("SB_LOG_DIR", test_log_dir());
    cmd
}
