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
    let got = toolchain::read_pins(&tree.env(), &token())
        .await
        .expect("toolchain");
    assert_eq!(got.python.as_deref(), Some(PYTHON));
    assert_eq!(got.minimum_uv.as_deref(), Some(UV));
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
    std::fs::write(&tree.layout.uv_pin, "0.99.0\n").expect("pin");
    std::fs::remove_file(&tree.layout.lock).expect("rm lock");
    let err = toolchain::read_pins(&tree.env(), &token())
        .await
        .expect_err("too old");
    assert_eq!(
        err.to_string(),
        format!(
            "saltbox requires uv 0.99.0 or newer, but this sb release provides {UV}; run sb self-update"
        )
    );
}

#[tokio::test]
async fn a_missing_lock_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.lock).expect("rm lock");
    let err = toolchain::read_pins(&tree.env(), &token())
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
    let err = toolchain::read_pins(&env, &token())
        .await
        .expect_err("no commit");
    let text = err.to_string();
    assert!(text.starts_with("read Saltbox commit: "), "{text}");
    assert!(text.contains("fatal: not a git repository"), "{text}");
}

/// A checkout without pins (Saltbox's pre-* branches) reads as none,
/// and without `.uv-version` no minimum is checked, even against an
/// older uv (ADR-031, D7).
#[tokio::test]
async fn a_checkout_without_pins_checks_no_minimum() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.python_pin).expect("rm pin");
    std::fs::remove_file(&tree.layout.uv_pin).expect("rm pin");
    let mut env = tree.env();
    env.uv_version = "0.0.1";
    let got = toolchain::read_pins(&env, &token()).await.expect("no pins");
    assert_eq!(got.python, None);
    assert_eq!(got.minimum_uv, None);
    assert_eq!(got.commit, COMMIT);
}

/// A Python pin without `.uv-version`: the exact pin, no minimum.
#[tokio::test]
async fn a_python_pin_without_a_uv_minimum_is_read_alone() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.uv_pin).expect("rm pin");
    let mut env = tree.env();
    env.uv_version = "0.0.1";
    let got = toolchain::read_pins(&env, &token())
        .await
        .expect("the Python pin");
    assert_eq!(got.python.as_deref(), Some(PYTHON));
    assert_eq!(got.minimum_uv, None);
}

/// A pin that is there but malformed is still refused, with today's
/// text, whether or not the other is there.
#[tokio::test]
async fn a_malformed_pin_is_still_refused() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.uv_pin).expect("rm pin");
    std::fs::write(&tree.layout.python_pin, "3.12\n").expect("pin");
    let err = toolchain::read_pins(&tree.env(), &token())
        .await
        .expect_err("malformed");
    assert_eq!(
        err.to_string(),
        format!(
            "load Saltbox Python toolchain: {} must contain one exact major.minor.patch version, got \"3.12\"",
            tree.layout.python_pin.display()
        )
    );
    std::fs::remove_file(&tree.layout.python_pin).expect("rm pin");
    std::fs::write(&tree.layout.uv_pin, "latest\n").expect("pin");
    let err = toolchain::read_pins(&tree.env(), &token())
        .await
        .expect_err("malformed");
    assert!(
        err.to_string().ends_with(
            "/.uv-version must contain one exact major.minor.patch version, got \"latest\""
        ),
        "{err}"
    );
}

/// The uv calls the stand-in recorded, without the uv path and
/// `--no-config`.
fn uv_calls(tree: &Tree) -> Vec<String> {
    let uv = format!("{} --no-config ", tree.layout.uv.display());
    tree.calls_made()
        .iter()
        .filter_map(|call| call.strip_prefix(&uv).map(str::to_string))
        .collect()
}

/// Without a pin, the newest 3.12 of `CPython`'s default builds in the
/// pinned uv's download list, unhashed, after one list call and nothing
/// else (sb-go's `TestResolveToolchainWithoutPinsUsesNewestFallbackPatch`).
#[tokio::test]
async fn without_a_pin_the_newest_312_uv_lists_is_resolved() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.python_pin).expect("rm pin");
    let env = tree.env();
    let pins = toolchain::read_pins(&env, &token()).await.expect("pins");
    let got = toolchain::resolve(&env, pins.clone(), &token())
        .await
        .expect("resolved");
    assert_eq!(
        got,
        toolchain::Toolchain {
            python: PYTHON.to_string(),
            hashed: false,
            lock_sha256: pins.lock_sha256,
            commit: pins.commit,
        }
    );
    assert_eq!(
        uv_calls(&tree),
        ["python list --only-downloads --all-versions --output-format json"]
    );
}

/// uv's list failing names uv's own message and this sb's uv.
#[tokio::test]
async fn a_failing_download_list_names_uvs_message() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.python_pin).expect("rm pin");
    tree.fail("python-list");
    let env = tree.env();
    let pins = toolchain::read_pins(&env, &token()).await.expect("pins");
    let err = toolchain::resolve(&env, pins, &token())
        .await
        .expect_err("failed list");
    let text = err.to_string();
    assert!(
        text.starts_with(&format!("list the Pythons uv {UV} can install: ")),
        "{text}"
    );
    assert!(text.contains("uv: python-list failed (stand-in)"), "{text}");
}

/// A list that isn't uv's JSON, and one without a 3.12, are named.
#[tokio::test]
async fn an_unusable_download_list_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.python_pin).expect("rm pin");
    let env = tree.env();
    let pins = toolchain::read_pins(&env, &token()).await.expect("pins");
    tree.python_downloads("not json");
    let err = toolchain::resolve(&env, pins.clone(), &token())
        .await
        .expect_err("not json");
    assert!(
        err.to_string()
            .starts_with(&format!("parse the Pythons uv {UV} can install: ")),
        "{err}"
    );
    tree.python_downloads(
        r#"[{"version": "3.13.1", "implementation": "cpython", "variant": "default"}]"#,
    );
    let err = toolchain::resolve(&env, pins, &token())
        .await
        .expect_err("no 3.12");
    assert_eq!(
        err.to_string(),
        format!(
            "this Saltbox branch has no pinned Python, and this sb's uv ({UV}) offers no Python 3.12; run sb self-update"
        )
    );
}

/// uv and uvx each probed with `--no-config --version`.
#[tokio::test]
async fn the_pinned_uv_passes_the_probe() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let installed = uv::is_installed(&tree.env(), &token())
        .await
        .expect("probed");
    assert!(installed);
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

/// Another release of either is not the pinned uv (module build M18
/// installs it then; tests/uv.rs).
#[tokio::test]
async fn another_uv_or_uvx_release_is_not_installed() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.uv("0.13.3", UV);
    assert!(
        !uv::is_installed(&tree.env(), &token())
            .await
            .expect("probed")
    );
    tree.uv(UV, "0.14.0");
    assert!(
        !uv::is_installed(&tree.env(), &token())
            .await
            .expect("probed")
    );
}

#[tokio::test]
async fn a_missing_or_strange_uv_fails_the_probe() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let env = tree.env();
    let probe = async || uv::version_of(&env, &tree.layout.uv, "uv", &token()).await;
    let _ = stand_in(&tree.layout.wrappers, "uv", "echo 'astral 0.13.4'");
    let err = probe().await.expect_err("strange");
    assert_eq!(
        err.to_string(),
        "unexpected uv version output \"astral 0.13.4\""
    );
    assert!(!uv::is_installed(&env, &token()).await.expect("probed"));
    std::fs::remove_file(&tree.layout.uv).expect("rm uv");
    let err = probe().await.expect_err("missing");
    assert!(err.to_string().contains("could not run"), "{err}");
    assert!(!uv::is_installed(&env, &token()).await.expect("probed"));
    let _ = stand_in(&tree.layout.wrappers, "uv", "echo broken >&2; exit 3");
    let err = probe().await.expect_err("failing");
    assert!(err.to_string().contains("broken"), "{err}");
    assert!(!uv::is_installed(&env, &token()).await.expect("probed"));
}

/// A binary that fails its probe and can't be looked at stops the
/// install (sb-go's `error checking installed`): here its directory is a
/// file.
#[tokio::test]
async fn a_binary_that_cant_be_looked_at_is_an_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let mut layout = tree.layout.clone();
    let file = tree.root.path().join("a-file");
    std::fs::write(&file, "").expect("file");
    layout.uvx = file.join("uvx");
    let env = sb_python_env::testing::with_stand_ins(
        sb_python_env::Env::new(&layout, &tree.releases, UV, tree.env().arch),
        tree.root.path(),
    );
    let err = uv::is_installed(&env, &token())
        .await
        .expect_err("not a directory");
    assert!(
        err.to_string()
            .starts_with("error checking installed uvx: "),
        "{err}"
    );
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
    // The probes run at once, so they start in any order.
    let mut calls = tree.calls_made();
    calls.sort();
    let mut want = [
        format!("{py} -I --version"),
        format!("{py} -I -c import encodings, ssl, sqlite3, ctypes"),
        format!("{uv} --no-config pip check --python {py}"),
        format!("{py} -I -c import ansible, apprise, certbot"),
        format!("{bin}/ansible --version"),
        format!("{bin}/certbot --version"),
        format!("{bin}/apprise --version"),
    ];
    want.sort();
    assert_eq!(calls, want);
    // Only ansible gets the isolated config, an empty file while it runs.
    let configs = std::fs::read_to_string(tree.entry_points_log()).expect("entry points");
    let mut configs: Vec<&str> = configs.lines().collect();
    configs.sort_unstable();
    assert_eq!(
        configs,
        ["ansible empty-file", "apprise unset", "certbot unset"]
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

/// `program` made to wait, before it runs as it did, until all seven
/// probes of the environment check have started (each touches a file in
/// `arrived`), or 20 s have passed (then it fails): probes run one after
/// another would never all start.
fn gated(program: &std::path::Path, arrived: &std::path::Path) {
    let real = program.with_extension("real");
    std::fs::rename(program, &real).expect("move the stand-in");
    std::fs::create_dir_all(arrived).expect("arrivals");
    let (dir, real) = (arrived.display(), real.display());
    let script = format!(
        ": > '{dir}/'$$\nn=0\n\
         while [ \"$(ls '{dir}' | wc -l)\" -lt 7 ]; do\n\
         n=$((n + 1)); [ \"$n\" -gt 2000 ] && exit 9; sleep 0.01\n\
         done\nexec '{real}' \"$@\""
    );
    let name = program.file_name().expect("name").to_str().expect("utf-8");
    let _ = stand_in(program.parent().expect("parent"), name, &script);
}

/// The seven probes run at once (performance research, round 2, item 3):
/// each waits for the others to start.
#[tokio::test]
async fn the_probes_run_at_once() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let venv = tree.root.path().join("gen/venv");
    tree.venv(&venv);
    let arrived = tree.root.path().join("arrived");
    for program in [
        venv.join("bin/python3"),
        venv.join("bin/ansible"),
        venv.join("bin/certbot"),
        venv.join("bin/apprise"),
        tree.layout.uv.clone(),
    ] {
        gated(&program, &arrived);
    }
    let env = sb_python_env::Env {
        probes_at_once: Some(at_once(7)),
        ..tree.env()
    };
    venv::check_environment(&env, &venv, PYTHON, &token())
        .await
        .expect("healthy");
}

/// `n` probes at once.
fn at_once(n: usize) -> std::num::NonZeroUsize {
    std::num::NonZeroUsize::new(n).expect("at least one")
}

/// After a failure the probes not yet started don't run, as when the
/// check stopped at its first failure: on one CPU, a Python that won't
/// run is the only probe.
#[tokio::test]
async fn no_probe_starts_after_a_failure() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let venv = tree.root.path().join("gen/venv");
    tree.venv(&venv);
    let python = venv.join("bin/python3");
    let _ = stand_in(
        &venv.join("bin"),
        "python3",
        &format!("echo \"$0 $*\" >> '{}'\nexit 1", tree.calls().display()),
    );
    let env = sb_python_env::Env {
        probes_at_once: Some(at_once(1)),
        ..tree.env()
    };
    let text = venv::check_environment(&env, &venv, PYTHON, &token())
        .await
        .expect_err("unhealthy")
        .to_string();
    assert!(
        text.starts_with(&format!("run Python at {}: ", python.display())),
        "{text}"
    );
    assert_eq!(
        tree.calls_made(),
        [format!("{} -I --version", python.display())]
    );
}

/// After Ctrl-C no probe starts.
#[tokio::test]
async fn no_probe_starts_after_ctrl_c() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let venv = tree.root.path().join("gen/venv");
    tree.venv(&venv);
    let cancelled = token();
    cancelled.cancel();
    venv::check_environment(&tree.env(), &venv, PYTHON, &cancelled)
        .await
        .expect_err("interrupted");
    assert_eq!(tree.calls_made(), Vec::<String>::new());
}

/// `program` made to note in `log` when it starts (`+`) and ends (`-`),
/// 200 ms apart, before it runs as it did.
fn timed(program: &std::path::Path, log: &std::path::Path) {
    let real = program.with_extension("real");
    std::fs::rename(program, &real).expect("move the stand-in");
    let (log, real) = (log.display(), real.display());
    let script = format!("echo + >> '{log}'\nsleep 0.2\necho - >> '{log}'\nexec '{real}' \"$@\"");
    let name = program.file_name().expect("name").to_str().expect("utf-8");
    let _ = stand_in(program.parent().expect("parent"), name, &script);
}

/// No more probes run at once than `probes_at_once` (by default the
/// physical cores sb may run on): one at a time on one core, as before,
/// and two on two.
#[tokio::test]
async fn the_probes_at_once_are_capped() {
    let _spawns = SPAWNS.lock().await;
    for cap in [1, 2] {
        let tree = Tree::new();
        let venv = tree.root.path().join("gen/venv");
        tree.venv(&venv);
        let log = tree.root.path().join("running.log");
        for program in [
            venv.join("bin/python3"),
            venv.join("bin/ansible"),
            venv.join("bin/certbot"),
            venv.join("bin/apprise"),
            tree.layout.uv.clone(),
        ] {
            timed(&program, &log);
        }
        let env = sb_python_env::Env {
            probes_at_once: Some(at_once(cap)),
            ..tree.env()
        };
        venv::check_environment(&env, &venv, PYTHON, &token())
            .await
            .expect("healthy");
        let (mut running, mut most) = (0_usize, 0_usize);
        let marks = std::fs::read_to_string(&log).expect("log");
        for mark in marks.lines() {
            if mark == "+" {
                running += 1;
                most = most.max(running);
            } else {
                running -= 1;
            }
        }
        assert_eq!(marks.lines().count(), 14, "{marks}");
        assert_eq!(most, cap, "{marks}");
    }
}

/// With several probes failing, the first in the check's order is the
/// one named, however soon the others end: a Python reporting another
/// release, late, beside a failing `uv pip check` and `certbot`.
#[tokio::test]
async fn the_first_failure_in_order_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let venv = tree.root.path().join("gen/venv");
    tree.venv(&venv);
    let python = venv.join("bin/python3");
    let _ = stand_in(
        &venv.join("bin"),
        "python3",
        "case \"$2\" in --version) sleep 0.3; echo 'Python 3.12.6' ;; esac",
    );
    tree.fail("pip-check");
    let _ = stand_in(&venv.join("bin"), "certbot", "exit 1");
    let err = venv::check_environment(&tree.env(), &venv, PYTHON, &token())
        .await
        .expect_err("unhealthy");
    assert_eq!(
        err.to_string(),
        format!(
            "python at {} reports \"Python 3.12.6\", expected \"Python 3.12.7\"",
            python.display()
        )
    );

    // A Python that doesn't run at all, late, before the same failures.
    let _ = stand_in(&venv.join("bin"), "python3", "sleep 0.3; exit 1");
    let text = venv::check_environment(&tree.env(), &venv, PYTHON, &token())
        .await
        .expect_err("unhealthy")
        .to_string();
    assert!(
        text.starts_with(&format!("run Python at {}: ", python.display())),
        "{text}"
    );
}
