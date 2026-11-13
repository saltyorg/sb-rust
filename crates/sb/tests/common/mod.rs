//! Shared by the `sb` binary tests.

use std::path::PathBuf;

/// A log directory for every test run of sb: tests run as root here, and a
/// root run logs; it must never reach the host's `/var/log/sb`. It lives in
/// Cargo's temporary directory for integration tests (under `target/`), so
/// `cargo clean` removes it and no run leaves a folder in /tmp
/// (<https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-crates>).
pub fn test_log_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sb-test-logs")
}

/// The built `sb`, logging to [`test_log_dir`]. `assert_cmd`'s prelude adds
/// `.assert()` to it. `SB_TEST_BINARY`, an absolute path to an existing
/// file, runs another build of sb in every test that uses this (the
/// production glibc binary, docker plan Task 9); a bare name would go
/// through `PATH` and could find an installed sb, so it is refused.
/// scripts/check.sh unsets it.
pub fn sb() -> std::process::Command {
    let program = std::env::var_os("SB_TEST_BINARY")
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from)
        .inspect(|path| {
            assert!(
                path.is_absolute() && path.is_file(),
                "SB_TEST_BINARY must be an absolute path to sb, got {}",
                path.display()
            );
        })
        .unwrap_or_else(|| assert_cmd::cargo::cargo_bin("sb"));
    let mut cmd = std::process::Command::new(program);
    apply_sb_env(&mut cmd);
    cmd
}

/// The environment every sb test run gets, for `cmd` (sb itself, or a
/// program that starts it: `script`, `unshare`, `setpriv`); tests that
/// want other values set them after. `tests/isolation.rs` checks that
/// every test starting sb by its path calls this.
pub fn apply_sb_env(cmd: &mut std::process::Command) {
    cmd.env("SB_LOG_DIR", test_log_dir());
    // No test reads a real /opt/sb/config.toml; tests that want one set it.
    cmd.env("SB_CONFIG", test_log_dir().join("no-such-config.toml"));
    // Nor the real Saltbox checkout: a root with no Saltbox files; tests
    // that want some point it at their own. (Without an inventory the
    // theme file would be `/opt/sb/config.toml`: `SB_CONFIG` above keeps
    // it off the host, not this.)
    cmd.env("SB_SALTBOX_DIR", test_log_dir().join("no-saltbox"));
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
    // Nor the developer's editor: `sb edit` tests set a fake one.
    cmd.env_remove("VISUAL");
    cmd.env_remove("EDITOR");
    for name in [
        "DOCKER_API_VERSION",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
    ] {
        cmd.env_remove(name);
    }
    // No test reaches Cloudflare or Docker Hub: both checks go to a
    // loopback port nothing listens on unless a test points them at its
    // fake, with no proxy, certificate or zoneinfo settings of the
    // developer's.
    let refused = refused_url();
    cmd.env("CLOUDFLARE_BASE_URL", &refused);
    cmd.env("SB_DOCKERHUB_URL", &refused);
    // Nor the Saltbox Docker controller, which starts and stops the host's
    // containers: a refused URL and a unit file that doesn't exist, unless
    // a test points them at its fake and its own file.
    cmd.env("SB_DOCKER_CONTROLLER_URL", &refused);
    cmd.env(
        "SB_DOCKER_CONTROLLER_UNIT",
        test_log_dir().join("no-controller.service"),
    );
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "ZONEINFO",
    ] {
        cmd.env_remove(name);
        cmd.env_remove(name.to_lowercase());
    }
}

/// A loopback URL whose port was bound and closed, so a request is
/// refused at once. Another test could bind the port in between: a low
/// risk, and such a run would fail rather than reach a real service.
fn refused_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().expect("its address").port();
    format!("http://127.0.0.1:{port}/")
}
