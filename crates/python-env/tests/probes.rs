//! The toolchain, the uv probe and the health checks against stand-in
//! git, uv, python and entry points on a temporary tree (python-env spec,
//! Testing Strategy). Nothing here touches `/srv`, `/usr/local/bin` or
//! the network.

use sb_python_env::testing::{COMMIT, PYTHON, Tree, UV, stand_in};
use sb_python_env::{toolchain, uv, venv};
use tokio_util::sync::CancellationToken;

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over, so no other test's fork holds its write
/// descriptor when it runs (exec fails with ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn token() -> CancellationToken {
    CancellationToken::new()
}

#[tokio::test]
async fn the_toolchain_is_read_from_the_checkout() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let got = toolchain::read_toolchain(&tree.env(), &token())
        .await
        .expect("toolchain");
    assert_eq!(got.python, PYTHON);
    assert_eq!(got.minimum_uv, UV);
    assert_eq!(got.commit, COMMIT);
    let digest = toolchain::file_sha256(&tree.layout.lock).expect("digest");
    assert_eq!(got.lock_sha256, digest);
    assert_eq!(got.lock_sha256.len(), 64);
}

/// The minimum is checked against the uv this sb provides, before the
/// lock or git are read.
#[tokio::test]
async fn a_newer_uv_minimum_stops_the_toolchain() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::write(&tree.layout.uv_pin, "0.13.10\n").expect("pin");
    std::fs::remove_file(&tree.layout.lock).expect("rm lock");
    let err = toolchain::read_toolchain(&tree.env(), &token())
        .await
        .expect_err("too old");
    assert_eq!(
        err.to_string(),
        "saltbox requires uv 0.13.10 or newer, but this sb release provides 0.13.4; run sb self-update"
    );
}

#[tokio::test]
async fn a_missing_lock_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.lock).expect("rm lock");
    let err = toolchain::read_toolchain(&tree.env(), &token())
        .await
        .expect_err("no lock");
    assert!(
        err.to_string()
            .starts_with("hash Saltbox requirements lock: "),
        "{err}"
    );
}

#[tokio::test]
async fn a_failing_git_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let mut env = tree.env();
    env.git = stand_in(
        &tree.root.path().join("stand-ins"),
        "broken-git",
        "echo 'fatal: not a git repository' >&2; exit 128",
    );
    let err = toolchain::read_toolchain(&env, &token())
        .await
        .expect_err("no commit");
    let text = err.to_string();
    assert!(text.starts_with("read Saltbox commit: "), "{text}");
    assert!(text.contains("fatal: not a git repository"), "{text}");
}

#[tokio::test]
async fn a_missing_python_pin_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.python_pin).expect("rm pin");
    let err = toolchain::read_toolchain(&tree.env(), &token())
        .await
        .expect_err("no pin");
    assert!(
        err.to_string()
            .starts_with("load Saltbox Python toolchain: read Python version from "),
        "{err}"
    );
}

/// uv and uvx each probed with `--no-config --version`.
#[tokio::test]
async fn the_pinned_uv_passes_the_probe() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    uv::check_installed(&tree.env(), &token())
        .await
        .expect("pinned uv");
    // A version probe makes no cache (sb-go's `isVersionProbe`).
    assert!(!tree.layout.uv_cache.exists());
    let uv = tree.layout.uv.display();
    let uvx = tree.layout.uvx.display();
    assert_eq!(
        tree.calls_made(),
        [
            format!("{uv} --no-config --version"),
            format!("{uvx} --no-config --version"),
        ]
    );
}

#[tokio::test]
async fn another_uv_or_uvx_release_fails_the_probe() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.uv("0.13.3", UV);
    let err = uv::check_installed(&tree.env(), &token())
        .await
        .expect_err("old uv");
    assert_eq!(
        err.to_string(),
        format!(
            "{} reports version 0.13.3, expected 0.13.4",
            tree.layout.uv.display()
        )
    );
    tree.uv(UV, "0.14.0");
    let err = uv::check_installed(&tree.env(), &token())
        .await
        .expect_err("new uvx");
    assert_eq!(
        err.to_string(),
        format!(
            "{} reports version 0.14.0, expected 0.13.4",
            tree.layout.uvx.display()
        )
    );
}

#[tokio::test]
async fn a_missing_or_strange_uv_fails_the_probe() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let _ = stand_in(&tree.layout.wrappers, "uv", "echo 'astral 0.13.4'");
    let err = uv::check_installed(&tree.env(), &token())
        .await
        .expect_err("strange");
    assert_eq!(
        err.to_string(),
        "unexpected uv version output \"astral 0.13.4\""
    );
    std::fs::remove_file(&tree.layout.uv).expect("rm uv");
    let err = uv::check_installed(&tree.env(), &token())
        .await
        .expect_err("missing");
    assert!(err.to_string().contains("could not run"), "{err}");
    let _ = stand_in(&tree.layout.wrappers, "uv", "echo broken >&2; exit 3");
    let err = uv::check_installed(&tree.env(), &token())
        .await
        .expect_err("failing");
    assert!(err.to_string().contains("broken"), "{err}");
}

#[tokio::test]
async fn a_healthy_venv_passes_every_probe() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let venv = tree.root.path().join("gen/venv");
    tree.venv(&venv);
    venv::check_environment(&tree.env(), &venv, PYTHON, &token())
        .await
        .expect("healthy");
    let python = venv.join("bin/python3");
    let (py, bin, uv) = (
        python.display(),
        venv.join("bin").display().to_string(),
        tree.layout.uv.display(),
    );
    assert_eq!(
        tree.calls_made(),
        [
            format!("{py} -I --version"),
            format!("{py} -I -c import encodings, ssl, sqlite3, ctypes"),
            format!("{uv} --no-config pip check --python {py}"),
            format!("{py} -I -c import ansible, apprise, certbot"),
            format!("{bin}/ansible --version"),
            format!("{bin}/certbot --version"),
            format!("{bin}/apprise --version"),
        ]
    );
    // Only ansible gets the isolated config, an empty file while it runs.
    let configs = std::fs::read_to_string(tree.entry_points()).expect("entry points");
    assert_eq!(
        configs,
        "ansible empty-file\ncertbot unset\napprise unset\n"
    );
}

#[tokio::test]
async fn another_python_release_is_unhealthy() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let python = tree.root.path().join("py/bin/python3");
    tree.python(&python, "3.12.6");
    let err = venv::check_python(&tree.env(), &python, PYTHON, &token())
        .await
        .expect_err("other release");
    assert_eq!(
        err.to_string(),
        format!(
            "python at {} reports \"Python 3.12.6\", expected \"Python 3.12.7\"",
            python.display()
        )
    );
}

/// decision 3: a Python that can't import `ssl` fails the Python check.
#[tokio::test]
async fn a_python_without_ssl_is_unhealthy() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let dir = tree.root.path().join("py/bin");
    let python = stand_in(
        &dir,
        "python3",
        "case \"$3\" in *ssl*) echo 'ModuleNotFoundError: _ssl' >&2; exit 1 ;; esac\n\
         echo 'Python 3.12.7'",
    );
    let err = venv::check_python(&tree.env(), &python, PYTHON, &token())
        .await
        .expect_err("no ssl");
    let text = err.to_string();
    assert!(
        text.starts_with(&format!(
            "python at {} cannot import its standard library: ",
            python.display()
        )),
        "{text}"
    );
    assert!(text.contains("_ssl"), "{text}");
}

#[tokio::test]
async fn a_python_that_wont_run_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let python = tree.root.path().join("absent/python3");
    let err = venv::check_python(&tree.env(), &python, PYTHON, &token())
        .await
        .expect_err("absent");
    assert!(
        err.to_string()
            .starts_with(&format!("run Python at {}: ", python.display())),
        "{err}"
    );
}

/// Each later probe's failure, in sb-go's words.
#[tokio::test]
async fn each_failing_probe_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let venv = tree.root.path().join("gen/venv");
    let check = async || {
        venv::check_environment(&tree.env(), &venv, PYTHON, &token())
            .await
            .expect_err("unhealthy")
            .to_string()
    };

    tree.venv(&venv);
    let _ = stand_in(&tree.layout.wrappers, "uv", "echo 'conflict' >&2; exit 1");
    let text = check().await;
    assert!(
        text.starts_with("installed Python packages are incompatible: "),
        "{text}"
    );

    tree.uv(UV, UV);
    let _ = stand_in(
        &venv.join("bin"),
        "python3",
        "case \"$3\" in *ansible*) exit 1 ;; esac\necho 'Python 3.12.7'",
    );
    let text = check().await;
    assert!(
        text.starts_with("import Saltbox Python packages: "),
        "{text}"
    );

    tree.venv(&venv);
    let _ = stand_in(&venv.join("bin"), "certbot", "exit 1");
    let text = check().await;
    assert!(text.starts_with("run certbot health check: "), "{text}");

    tree.venv(&venv);
    std::fs::remove_file(venv.join("bin/apprise")).expect("rm apprise");
    let text = check().await;
    assert!(text.starts_with("run apprise health check: "), "{text}");
}
