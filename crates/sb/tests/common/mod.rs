//! Shared by the `sb` binary tests.

use std::fs::File;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A log directory for every test run of sb: tests run as root here, and a
/// root run logs; it must never reach the host's `/var/log/sb`. It lives in
/// Cargo's temporary directory for integration tests (under `target/`), so
/// `cargo clean` removes it and no run leaves a folder in /tmp
/// (<https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-crates>).
///
/// One per test process, in `sb-test-logs`, made root-only (0700), the
/// mode sb keeps its log directory in: sb then uses it as it is, whatever
/// else it holds (the run lock directories, [`run_lock_dir`]). A
/// directory sb found looser it would lock down only if it held nothing
/// but sb's logs, and warn on every run otherwise (cli-core's
/// `RunLog::open`), which broke the tests that compare stderr whenever a
/// lock directory got there first. Those of processes that have ended are
/// removed as a process makes its own ([`remove_ended`]).
pub fn test_log_dir() -> PathBuf {
    own_dir().to_path_buf()
}

/// Where the test processes' directories are.
fn test_logs() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sb-test-logs")
}

/// This test process's directory in [`test_logs`], made on first use: a
/// fresh name (tempfile's), mode 0700, and beside it `<name>.lock`,
/// locked for as long as the process lives (std's `File::lock`, an
/// `flock` the kernel releases when the process ends, however it ends,
/// and in any PID namespace, as the gate's sandbox has;
/// <https://doc.rust-lang.org/std/fs/struct.File.html#method.lock>).
/// Made under `.setup.lock`, so no process clears another's half-made
/// directory. Custom: a `TempDir` in a static is never dropped, so it
/// never removes itself, and neither tempfile nor std clears what ended
/// processes left.
fn own_dir() -> &'static Path {
    static OWN: OnceLock<(PathBuf, File)> = OnceLock::new();
    let (dir, _held) = OWN.get_or_init(|| {
        let logs = test_logs();
        std::fs::create_dir_all(&logs).expect("test logs");
        let setup = File::create(logs.join(".setup.lock")).expect("setup lock");
        setup.lock().expect("setup lock");
        remove_ended(&logs);
        let dir = tempfile::Builder::new()
            .prefix("run-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(&logs)
            .expect("test log dir")
            .keep();
        let held = File::create(lock_of(&dir)).expect("held lock");
        held.lock().expect("held lock");
        (dir, held)
    });
    dir
}

/// The lock file beside a process's directory `dir`.
fn lock_of(dir: &Path) -> PathBuf {
    let mut name = dir.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Remove from `logs` what test processes that have ended left: a
/// directory whose lock can be taken, with its lock, and anything else no
/// held lock speaks for (an earlier layout's `sb.log` and `run-lock-*`, a
/// lock whose directory is gone). Called under `.setup.lock`.
fn remove_ended(logs: &Path) {
    let free = |lock: &Path| File::open(lock).is_ok_and(|lock| lock.try_lock().is_ok());
    for entry in std::fs::read_dir(logs).expect("test logs").flatten() {
        let path = entry.path();
        if entry.file_name() == ".setup.lock" {
            continue;
        }
        if path.extension().is_some_and(|ext| ext == "lock") {
            if !path.with_extension("").exists() && free(&path) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        let lock = lock_of(&path);
        if !lock.exists() || free(&lock) {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
            let _ = std::fs::remove_file(&lock);
        }
    }
}

/// A run lock directory no other run of sb uses: a count, in this test
/// process's [`test_log_dir`] (sb makes the lock's own directory, never
/// its parent).
fn run_lock_dir() -> PathBuf {
    static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    test_log_dir().join(format!("run-lock-{run}"))
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
    // off on a terminal), nor their colour switches: `NO_COLOR` takes
    // colour away, `CLICOLOR=0` too on a terminal, and `CLICOLOR_FORCE`
    // colours piped output, as anstream and console read them
    // (<https://docs.rs/anstream/1.0.0/anstream/struct.AutoStream.html#method.choice>,
    // <https://docs.rs/console/0.16.6/console/fn.colors_enabled.html>);
    // tests that want one set it.
    for name in ["SB_COLOR_PROFILE", "NO_COLOR", "CLICOLOR", "CLICOLOR_FORCE"] {
        cmd.env_remove(name);
    }
    // Nor the developer's login shell: util-linux `script` runs its
    // command with `$SHELL -c` (script(1), ENVIRONMENT,
    // <https://man7.org/linux/man-pages/man1/script.1.html>), and the
    // tests' commands are POSIX shell; tests that want another shell set
    // it.
    cmd.env("SHELL", "/bin/sh");
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
