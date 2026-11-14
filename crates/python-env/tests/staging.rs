//! The build into staging (python-env spec, steps 7-11 and 17) on a
//! stand-in tree: the stand-in uv installs a stand-in Python, makes a
//! venv and syncs stand-in entry points from the tree's templates.
//! Nothing here touches `/srv`, `/usr/local/bin`, apt or the network.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use sb_cli_core::cancel::Shutdown;
use sb_python_env::testing::{PYTHON, Tree};
use sb_python_env::toolchain::{self, Toolchain};
use sb_python_env::venv::{Staged, build};
use sb_python_env::{Force, manifest};
use sb_term_ui::{Labels, Options, Runner, Step, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What a run left: its body's error chain, if any, and the screen.
struct Ran<T> {
    value: Option<T>,
    error: Option<String>,
    screen: String,
}

/// `body` in a step of a runner drawing to memory; `body` gets the
/// shutdown too, to signal Ctrl-C.
async fn in_step<T>(body: impl AsyncFnOnce(&mut Step, &Shutdown) -> anyhow::Result<T>) -> Ran<T> {
    let term = indicatif::InMemoryTerm::new(80, 400);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb test", &shutdown, None, options);
    let mut error = None;
    let labels = Labels::new(
        "Preparing Ansible virtual environment",
        "Ansible virtual environment ready",
    );
    let value = runner
        .step(labels, async |step| {
            let result = body(step, &shutdown).await;
            error = result.as_ref().err().map(|err| format!("{err:#}"));
            result
        })
        .await
        .ok();
    runner.finish();
    Ran {
        value,
        error,
        screen: term.contents(),
    }
}

/// The tree's toolchain, as `reconcile_venv` reads it.
async fn toolchain(tree: &Tree) -> Toolchain {
    let token = tokio_util::sync::CancellationToken::new();
    toolchain::read_toolchain(&tree.env(), &token)
        .await
        .expect("toolchain")
}

/// The uv calls made, without the uv path and `--no-config`.
fn uv_calls(tree: &Tree) -> Vec<String> {
    let uv = format!("{} --no-config ", tree.layout.uv.display());
    tree.calls_made()
        .into_iter()
        .filter_map(|call| call.strip_prefix(&uv).map(str::to_string))
        .collect()
}

/// The entries of `dir`, by name (none when it is missing).
fn entries(dir: &Path) -> Vec<String> {
    // A missing directory has none.
    let mut names: Vec<String> = std::fs::read_dir(dir).map_or_default(|entries| {
        entries
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    });
    names.sort();
    names
}

/// A first build: a new Python release, the venv, the sync, the checks
/// and the manifest, with sb-go's argv and labels; dropped, it goes.
#[tokio::test]
async fn a_first_build_makes_a_python_release_and_a_generation() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let toolchain = toolchain(&tree).await;
    let env = tree.env();
    let ran = in_step(async |step, _| {
        let staged = build(step, &env, &toolchain, None, Force::None).await?;
        Ok((staged.generation.clone(), staged.manifest.clone()))
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    for line in [
        "Installing Python 3.12.7",
        "Creating Ansible virtual environment",
        "Syncing hashed Saltbox requirements",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    let (generation, written) = ran.value.expect("built");
    let id = generation
        .file_name()
        .expect("id")
        .to_string_lossy()
        .into_owned();
    let (stamp, digest) = id.split_once('-').expect("stamp-digest");
    assert_eq!(digest, &toolchain.lock_sha256[..12]);
    assert_eq!(stamp.len(), "20260807T120000.000000000".len(), "{stamp}");
    assert_eq!(written.generation_id, id);
    assert_eq!(written.python_version, PYTHON);
    assert_eq!(written.uv_version, "0.13.4");
    assert_eq!(written.saltbox_commit, toolchain.commit);
    assert_eq!(written.exported_commands, ["ansible", "apprise", "certbot"]);
    let install = Path::new(&written.python_install);
    let release = install
        .file_name()
        .expect("release")
        .to_string_lossy()
        .into_owned();
    assert!(release.starts_with("python-3-12-7-"), "{release}");
    assert_eq!(
        written.python_path,
        install.join("cpython/bin/python3.12").display().to_string()
    );
    let venv = generation.join("venv");
    let (dir, py) = (install.display(), venv.join("bin/python3"));
    let calls = uv_calls(&tree);
    assert_eq!(
        calls[..4],
        [
            format!("python install --managed-python --no-bin --install-dir {dir} 3.12.7"),
            "python find --managed-python --no-project --no-python-downloads 3.12.7".to_string(),
            format!(
                "venv --python {} --no-project --no-python-downloads {}",
                written.python_path,
                venv.display()
            ),
            format!(
                "pip sync --no-progress --python {} --require-hashes {}",
                py.display(),
                tree.layout.lock.display()
            ),
        ]
    );
    // The staged check ran on the new venv: uv's pip check on its Python.
    assert!(
        calls.contains(&format!("pip check --python {}", py.display())),
        "{calls:?}"
    );
    // Dropped (no activation), the generation and the new Python go.
    assert!(!generation.exists());
    assert!(!install.exists());
}

/// The manifest is schema 1, written whole through a temporary file and
/// renamed, 0644 (spec, step 11); kept, the build stays.
#[tokio::test]
async fn a_kept_build_holds_its_manifest() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let toolchain = toolchain(&tree).await;
    let env = tree.env();
    let ran = in_step(async |step, _| {
        let staged: Staged<'_> = build(step, &env, &toolchain, None, Force::None).await?;
        let generation = staged.generation.clone();
        Ok((generation, staged.keep()))
    })
    .await;
    let (generation, kept) = ran.value.expect("built");
    assert_eq!(entries(&generation), ["manifest.json", "venv"]);
    let read = manifest::read(&generation)
        .expect("read")
        .expect("complete");
    assert_eq!(read, kept);
    assert_eq!(read.schema_version, 1);
    let mode = std::fs::metadata(generation.join("manifest.json"))
        .expect("manifest")
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o644);
    assert!(Path::new(&kept.python_install).exists());
}

/// The active Python is reused when it is the pinned one and passes the
/// Python check: no install, and a failed build leaves it alone.
#[tokio::test]
async fn the_active_python_is_reused() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let toolchain = toolchain(&tree).await;
    let env = tree.env();
    for force in [Force::None, Force::Venv] {
        let ran = in_step(async |step, _| {
            let staged = build(step, &env, &toolchain, Some(&active.manifest), force).await?;
            Ok(staged.manifest.clone())
        })
        .await;
        let written = ran.value.expect("built");
        assert!(!ran.screen.contains("Installing Python"), "{}", ran.screen);
        assert_eq!(written.python_path, active.manifest.python_path);
        assert_eq!(written.python_install, active.manifest.python_install);
    }
    // Both builds gone, the active generation and its Python kept.
    assert_eq!(
        entries(&tree.layout.ansible_releases),
        [active.dir.file_name().expect("id").to_string_lossy()]
    );
    assert!(active.python_install.exists());
    // `--no-cache` only when forced (sb-go 7d1ee0e).
    let syncs: Vec<String> = uv_calls(&tree)
        .into_iter()
        .filter(|call| call.starts_with("pip sync"))
        .collect();
    assert!(!syncs[0].contains("--no-cache"), "{syncs:?}");
    assert!(
        syncs[1].contains("--require-hashes --no-cache "),
        "{syncs:?}"
    );
}

/// `reinstall-python`'s force: a new release with `--reinstall` and
/// `--no-cache`, whatever the active one.
#[tokio::test]
async fn a_python_force_installs_afresh() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let toolchain = toolchain(&tree).await;
    let env = tree.env();
    let ran = in_step(async |step, _| {
        let staged = build(
            step,
            &env,
            &toolchain,
            Some(&active.manifest),
            Force::Python,
        )
        .await?;
        Ok(staged.manifest.clone())
    })
    .await;
    let written = ran.value.expect("built");
    assert_ne!(written.python_install, active.manifest.python_install);
    let install = uv_calls(&tree)
        .into_iter()
        .find(|call| call.starts_with("python install"))
        .expect("install");
    assert!(
        install.ends_with("--reinstall --no-cache 3.12.7"),
        "{install}"
    );
}

/// `reinstall-venv`'s force with no usable Python: a new release with
/// `--no-cache` but no `--reinstall` (sb-go's
/// `TestCleanReinstallBypassesUVCache`, "Python install during forced venv
/// recovery").
#[tokio::test]
async fn a_venv_force_installs_without_reinstall() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let toolchain = toolchain(&tree).await;
    let env = tree.env();
    let ran = in_step(async |step, _| {
        let _staged = build(step, &env, &toolchain, None, Force::Venv).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let install = uv_calls(&tree)
        .into_iter()
        .find(|call| call.starts_with("python install"))
        .expect("install");
    assert!(install.ends_with(" --no-cache 3.12.7"), "{install}");
    assert!(!install.contains("--reinstall"), "{install}");
}

/// A reused Python must pass the Python check, or a new one is installed.
#[tokio::test]
async fn a_broken_active_python_is_replaced() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    tree.python(Path::new(&active.manifest.python_path), "3.12.6");
    let toolchain = toolchain(&tree).await;
    let env = tree.env();
    let ran = in_step(async |step, _| {
        let staged = build(step, &env, &toolchain, Some(&active.manifest), Force::None).await?;
        Ok(staged.manifest.clone())
    })
    .await;
    let written = ran.value.expect("built");
    assert!(
        ran.screen.contains("Installing Python 3.12.7"),
        "{}",
        ran.screen
    );
    assert_ne!(written.python_path, active.manifest.python_path);
}

/// A failure at each step leaves the active link, the wrappers and the
/// previous generation as they were, and removes what the build made
/// (spec, step 17; success criterion 5).
#[tokio::test]
async fn a_failed_build_leaves_nothing_behind() {
    let _spawns = SPAWNS.lock().await;
    let cases: [(&str, &str); 4] = [
        ("python-install", "error installing Python 3.12.7"),
        ("venv---python", "uv: venv---python failed"),
        ("pip-sync", "uv: pip-sync failed"),
        (
            "pip-check",
            "validate staged Ansible environment: installed Python packages are incompatible",
        ),
    ];
    for (op, want) in cases {
        let tree = Tree::new();
        let active = tree.healthy();
        // Not the active Python, so a new release is made too.
        tree.python(Path::new(&active.manifest.python_path), "3.12.6");
        let wrapper = tree.layout.wrappers.join("ansible");
        std::fs::write(&wrapper, "before").expect("wrapper");
        tree.fail(op);
        let toolchain = toolchain(&tree).await;
        let env = tree.env();
        let ran = in_step(async |step, _| {
            let _staged =
                build(step, &env, &toolchain, Some(&active.manifest), Force::None).await?;
            Ok(())
        })
        .await;
        assert!(ran.value.is_none(), "{op}: built");
        assert!(
            ran.screen.contains(want) || ran.error.as_deref().is_some_and(|e| e.contains(want)),
            "{op}: {:?}\n{}",
            ran.error,
            ran.screen
        );
        assert_eq!(
            std::fs::read_link(&tree.layout.active_venv).expect("link"),
            active.venv,
            "{op}"
        );
        assert_eq!(
            std::fs::read_to_string(&wrapper).expect("wrapper"),
            "before",
            "{op}"
        );
        assert_eq!(
            entries(&tree.layout.ansible_releases),
            [active.dir.file_name().expect("id").to_string_lossy()],
            "{op}"
        );
        assert_eq!(
            entries(&tree.layout.python_releases),
            [active
                .python_install
                .file_name()
                .expect("id")
                .to_string_lossy()],
            "{op}"
        );
    }
}

/// Discovery's rules on the staged venv: the three entry points are
/// required.
#[tokio::test]
async fn a_venv_without_certbot_is_refused() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(tree.root.path().join("templates/bin/certbot")).expect("rm");
    let toolchain = toolchain(&tree).await;
    let env = tree.env();
    let ran = in_step(async |step, _| {
        let _staged = build(step, &env, &toolchain, None, Force::None).await?;
        Ok(())
    })
    .await;
    assert_eq!(
        ran.error.as_deref(),
        Some("required command certbot is missing from staged venv")
    );
    assert_eq!(entries(&tree.layout.ansible_releases), Vec::<String>::new());
    assert_eq!(entries(&tree.layout.python_releases), Vec::<String>::new());
}

/// uv's answer must be an absolute path (sb-go's text).
#[tokio::test]
async fn a_relative_python_from_uv_is_refused() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.find_answer("cpython/bin/python3.12");
    let toolchain = toolchain(&tree).await;
    let env = tree.env();
    let ran = in_step(async |step, _| {
        let _staged = build(step, &env, &toolchain, None, Force::None).await?;
        Ok(())
    })
    .await;
    assert_eq!(
        ran.error.as_deref(),
        Some("uv returned non-absolute path \"cpython/bin/python3.12\" for Python 3.12.7")
    );
    assert_eq!(entries(&tree.layout.python_releases), Vec::<String>::new());
}

/// Wait for `path`, up to 20 s, so a build that fails before its hang
/// fails the test with a message.
async fn appears(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Ctrl-C during `python install` and during `pip sync`, which have no
/// timeout: the step is interrupted, the child ends within exec's grace
/// and a second (spec, success criterion 9), what the build made goes and
/// the active venv stays (spec, step 17).
#[tokio::test]
async fn a_cancelled_build_is_cleaned_up() {
    let _spawns = SPAWNS.lock().await;
    for op in ["python-install", "pip-sync"] {
        let tree = Tree::new();
        let active = tree.healthy();
        // Not the active Python, so the build installs a new one.
        tree.python(Path::new(&active.manifest.python_path), "3.12.6");
        tree.hang(op);
        let toolchain = toolchain(&tree).await;
        let env = tree.env();
        let started = tree.root.path().join("hang/started");
        let mut cancelled_at = None;
        let ran = in_step(async |step, shutdown| {
            let cancel = async {
                appears(&started).await;
                cancelled_at = Some(std::time::Instant::now());
                shutdown.signalled(2);
            };
            let (built, ()) = tokio::join!(
                build(step, &env, &toolchain, Some(&active.manifest), Force::None),
                cancel
            );
            let _staged = built?;
            Ok(())
        })
        .await;
        let ended = cancelled_at.expect("cancelled").elapsed();
        assert!(ended < std::time::Duration::from_secs(6), "{op}: {ended:?}");
        assert!(ran.value.is_none(), "{op}");
        assert!(ran.screen.contains("interrupted"), "{op}:\n{}", ran.screen);
        assert_eq!(
            entries(&tree.layout.ansible_releases),
            [active.dir.file_name().expect("id").to_string_lossy()],
            "{op}"
        );
        assert_eq!(
            entries(&tree.layout.python_releases),
            [active
                .python_install
                .file_name()
                .expect("id")
                .to_string_lossy()],
            "{op}"
        );
        assert_eq!(
            std::fs::read_link(&tree.layout.active_venv).expect("link"),
            active.venv,
            "{op}"
        );
    }
}
