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

/// A run lock directory no other run of sb uses: this test process's id
/// and a count, in [`test_log_dir`], made here (sb makes the lock's own
/// directory, never its parent).
fn run_lock_dir() -> PathBuf {
    static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::fs::create_dir_all(test_log_dir()).expect("test log dir");
    test_log_dir().join(format!("run-lock-{}-{run}", std::process::id()))
}

/// The built `sb`, logging to [`test_log_dir`]. `assert_cmd`'s prelude adds
/// `.assert()` to it. `SB_TEST_BINARY`, an absolute path to an existing
/// file, runs another build of sb in every test that uses this (the
/// production glibc binary, docker plan Task 9); a bare name would go
/// through `PATH` and could find an installed sb, so it is refused.
/// scripts/check.sh unsets it. Such a build has no test seams of its
/// own: its locking commands take the host's run lock in `/run/sb`
/// (cli-core spec, "Run lock"), so tests that hold the lock use this test
/// build.
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
    // Help wraps at the width COLUMNS gives when stdout isn't a terminal
    // (cli-core spec, Help): none of the developer's; tests that want one
    // set it.
    cmd.env_remove("COLUMNS");
    // No test reads a real /opt/sb/config.toml; tests that want one set it.
    cmd.env("SB_CONFIG", test_log_dir().join("no-such-config.toml"));
    // Nor the real Saltbox checkout: a root with no Saltbox files; tests
    // that want some point it at their own. (Without an inventory the
    // theme file would be `/opt/sb/config.toml`: `SB_CONFIG` above keeps
    // it off the host, not this.)
    cmd.env("SB_SALTBOX_DIR", test_log_dir().join("no-saltbox"));
    // Nor the host's backups: motd.yml's migration copies the original
    // under the test's directory; tests that read the copy set their own.
    cmd.env("SB_BACKUPS_DIR", test_log_dir().join("backups"));
    // Nor the host's venv, wrappers, uv or apt: the test build's python-env
    // layout (its `testing` feature) under an empty root; tests that want
    // a stand-in tree point it at their own.
    cmd.env(
        "SB_TEST_PYTHON_ENV_ROOT",
        test_log_dir().join("no-python-env"),
    );
    // No test reaches the host's Docker daemon: a socket that doesn't
    // exist unless a test points DOCKER_HOST at its fake, and none of the
    // developer's Docker settings.
    cmd.env(
        "DOCKER_HOST",
        format!("unix://{}", test_log_dir().join("no-docker.sock").display()),
    );
    // Nor the host's journal: a directory that doesn't exist, unless a
    // test points `SB_JOURNAL_DIR` at its own crafted files.
    cmd.env("SB_JOURNAL_DIR", test_log_dir().join("no-journal"));
    // Nor Ansible: a playbook program that doesn't exist, unless a test
    // points it at its fake.
    cmd.env(
        "SB_ANSIBLE_PLAYBOOK",
        test_log_dir().join("no-ansible-playbook"),
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
    // Nor the host's shell directories: `sb completion install` writes its
    // files under a root of the test's (the test build's
    // `SB_TEST_COMPLETION_ROOT`); tests that check them set their own.
    cmd.env(
        "SB_TEST_COMPLETION_ROOT",
        test_log_dir().join("no-completion-root"),
    );
    // Nor the host's run lock (`/run/sb`): a lock directory of this run's
    // own, so tests running at once never wait for each other; the lock's
    // tests share one between their runs.
    cmd.env("SB_TEST_RUN_LOCK_DIR", run_lock_dir());
    // No test reaches Cloudflare or Docker Hub: both checks go to a
    // loopback port nothing listens on unless a test points them at its
    // fake, with no proxy, certificate or zoneinfo settings of the
    // developer's.
    let refused = refused_url();
    cmd.env("CLOUDFLARE_BASE_URL", refused);
    cmd.env("SB_DOCKERHUB_URL", refused);
    // Nor the Saltbox Docker controller, which starts and stops the host's
    // containers: a refused URL and a unit file that doesn't exist, unless
    // a test points them at its fake and its own file.
    cmd.env("SB_DOCKER_CONTROLLER_URL", refused);
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

/// A loopback URL nothing listens on, the same for the whole test binary:
/// its port is held by a socket that lives as long as the process (a
/// static; `apply_sb_env`'s callers build commands in many places and
/// couldn't each keep a socket), bound and never listening, so a request
/// is refused at once (connect(2), `ECONNREFUSED`,
/// <https://man7.org/linux/man-pages/man2/connect.2.html>) and no other
/// test's server can be given the port, as a port bound and freed again
/// could be. tokio's `TcpSocket` binds without listening
/// (<https://docs.rs/tokio/1.54.1/tokio/net/struct.TcpSocket.html#method.bind>);
/// std's `TcpListener::bind` always listens. The port stays held because
/// the socket has no `SO_REUSEADDR` (tokio's `TcpSocket` leaves it off),
/// and Linux lets a second socket share the address only when both set it
/// (socket(7), <https://man7.org/linux/man-pages/man7/socket.7.html>);
/// the assertion below checks it.
fn refused_url() -> &'static str {
    static HELD: std::sync::OnceLock<(tokio::net::TcpSocket, String)> = std::sync::OnceLock::new();
    let (_socket, url) = HELD.get_or_init(|| {
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        socket
            .bind(std::net::SocketAddr::from((
                std::net::Ipv4Addr::LOCALHOST,
                0,
            )))
            .expect("bind");
        let addr = socket.local_addr().expect("addr");
        // The port is held: nothing else can bind it while the socket lives.
        let second = std::net::TcpListener::bind(addr).map(|_| ());
        assert_eq!(
            second.map_err(|e| e.kind()),
            Err(std::io::ErrorKind::AddrInUse),
            "{addr} not held"
        );
        (socket, format!("http://{addr}/"))
    });
    url
}
