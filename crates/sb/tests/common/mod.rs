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
    // No test reads a real /opt/sb/config.toml; tests that want one set it.
    cmd.env("SB_CONFIG", test_log_dir().join("no-such-config.toml"));
    // No test reaches the host's Docker daemon: a socket that doesn't
    // exist unless a test points DOCKER_HOST at its fake, and none of the
    // developer's Docker settings.
    cmd.env(
        "DOCKER_HOST",
        format!("unix://{}", test_log_dir().join("no-docker.sock").display()),
    );
    // Nor the developer's colour profile (an `ascii` one would turn colour
    // off on a terminal); tests that want one set it.
    cmd.env_remove("SB_COLOR_PROFILE");
    for name in [
        "DOCKER_API_VERSION",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
    ] {
        cmd.env_remove(name);
    }
    cmd
}
