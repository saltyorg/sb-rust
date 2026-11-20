//! `sb update --verify-venv` through `reconcile_venv` (python-env spec,
//! decision 25): with `Env::verify`, the active venv's files against
//! their RECORDs and its Python release's against sb's manifest, in
//! "Verifying Ansible virtual environment files"; damage rebuilds the
//! venv, or the Python, with uv's cache cleared first. Nothing here
//! touches `/srv`, `/usr/local/bin`, apt or the network: every program is
//! a stand-in on a temporary root.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use sb_cli_core::cancel::Shutdown;
use sb_python_env::testing::{Generation, Tree};
use sb_python_env::{Force, VenvOutcome, reconcile_venv, verify};
use sb_term_ui::{Labels, Options, Runner, Step, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Ran {
    error: Option<String>,
    screen: String,
}

async fn in_step(body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<()>) -> Ran {
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
    let _ = runner
        .step(labels, async |step| {
            let result = body(step).await;
            error = result.as_ref().err().map(|err| format!("{err:#}"));
            result
        })
        .await;
    runner.finish();
    Ran {
        error,
        screen: term.contents(),
    }
}

/// `venv`'s one wheel, `pkg`: a module and its RECORD.
fn wheel(venv: &Path) -> PathBuf {
    let site = venv.join("lib/python3.12/site-packages");
    std::fs::create_dir_all(site.join("pkg")).expect("pkg");
    std::fs::create_dir_all(site.join("pkg-1.0.dist-info")).expect("dist-info");
    let module = site.join("pkg/__init__.py");
    let bytes = b"VALUE = 1\n";
    std::fs::write(&module, bytes).expect("module");
    let sha = sb_release::sha256_reader(&bytes[..]).expect("digest");
    let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha);
    std::fs::write(
        site.join("pkg-1.0.dist-info/RECORD"),
        format!(
            "pkg/__init__.py,sha256={digest},{}\npkg-1.0.dist-info/RECORD,,\n",
            bytes.len()
        ),
    )
    .expect("record");
    module
}

/// A healthy tree whose venv has a wheel, its module's path.
fn tree() -> (Tree, Generation, PathBuf) {
    let tree = Tree::new();
    let active = tree.healthy();
    let module = wheel(&active.venv);
    (tree, active, module)
}

async fn run(tree: &Tree, verify: bool) -> (Ran, Option<VenvOutcome>) {
    let mut env = tree.env();
    env.verify = verify;
    let mut outcome = None;
    let ran = in_step(async |step| {
        outcome = Some(reconcile_venv(step, &env, None, Force::None).await?);
        Ok(())
    })
    .await;
    (ran, outcome)
}

/// The uv calls made, without the uv path and `--no-config`.
fn uv_calls(tree: &Tree) -> Vec<String> {
    let uv = format!("{} --no-config ", tree.layout.uv.display());
    tree.calls_made()
        .into_iter()
        .filter_map(|call| call.strip_prefix(&uv).map(str::to_string))
        .collect()
}

/// sb's euid, whose manifests are trusted.
fn me() -> u32 {
    sb_cli_core::root::effective_uid()
}

const VERIFIED: &str = "Ansible virtual environment files verified";

#[tokio::test]
async fn without_the_flag_nothing_is_verified() {
    let _spawns = SPAWNS.lock().await;
    let (tree, _, module) = tree();
    std::fs::write(&module, "VALUE = 2\n").expect("poison");
    let (ran, outcome) = run(&tree, false).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(outcome, Some(VenvOutcome::UpToDate));
    assert!(!ran.screen.contains("Verifying"), "{}", ran.screen);
}

#[tokio::test]
async fn a_clean_venv_is_verified_and_kept() {
    let _spawns = SPAWNS.lock().await;
    let (tree, _, _) = tree();
    let (ran, outcome) = run(&tree, true).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(outcome, Some(VenvOutcome::UpToDate));
    for line in [
        VERIFIED,
        "Packages: 1 file checked, 0 bad, 0 missing",
        "Python: not checked, no manifest yet (sb writes one when it installs the Python: sb reinstall-python)",
        "Ansible virtual environment was already up to date",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    let inspected = ran
        .screen
        .find("Active Ansible virtual environment inspected");
    let verified = ran.screen.find(VERIFIED);
    let reported = ran.screen.find("already up to date");
    assert!(
        inspected < verified && verified < reported,
        "{}",
        ran.screen
    );
}

/// A package file changed in place: the venv is rebuilt on its Python,
/// uv's cache cleared first (its hardlinks may share the damage).
#[tokio::test]
async fn a_poisoned_package_rebuilds_the_venv_with_the_cache_cleared() {
    let _spawns = SPAWNS.lock().await;
    let (tree, active, module) = tree();
    std::fs::write(&module, "VALUE = 2\n").expect("poison");
    let (ran, outcome) = run(&tree, true).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(outcome, Some(VenvOutcome::Rebuilt));
    for line in [
        "Packages: 1 file checked, 1 bad, 0 missing",
        "update required",
        "uv cache cleared",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    let calls = uv_calls(&tree);
    assert!(
        calls.iter().any(|call| call.starts_with("cache clean")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|call| call.starts_with("python install")),
        "the Python is reused: {calls:?}"
    );
    assert!(
        !active.dir.exists(),
        "the damaged generation is cleaned away"
    );
}

/// A stdlib file differing from sb's manifest: the Python is reinstalled
/// and the venv rebuilt on it; the new release gets its own manifest,
/// 0644, and the old one's goes with its release.
#[tokio::test]
async fn a_damaged_stdlib_rebuilds_the_python_and_writes_its_manifest() {
    let _spawns = SPAWNS.lock().await;
    let (tree, active, _) = tree();
    let old_manifest = verify::manifest_path(&tree.layout, &active.python_install);
    let lib = active.python_install.join("cpython/lib/python3.12");
    std::fs::create_dir_all(&lib).expect("lib");
    let stdlib = lib.join("smtplib.py");
    std::fs::write(&stdlib, "import socket\n").expect("stdlib");
    verify::write_release_manifest(&active.python_install, &old_manifest, me()).expect("manifest");
    std::fs::write(&stdlib, "import sockex\n").expect("poison");
    let (ran, outcome) = run(&tree, true).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(outcome, Some(VenvOutcome::Rebuilt));
    assert!(
        ran.screen
            .contains("Packages: 1 file checked, 0 bad, 0 missing")
            && ran
                .screen
                .contains("Python: 2 files checked, 1 bad, 0 missing"),
        "{}",
        ran.screen
    );
    let calls = uv_calls(&tree);
    let install = calls
        .iter()
        .find(|call| call.starts_with("python install"))
        .unwrap_or_else(|| panic!("{calls:?}"));
    assert!(install.contains(" --reinstall --no-cache "), "{install}");
    assert!(!active.python_install.exists());
    assert!(!old_manifest.exists(), "its manifest goes with it");
    let pythons: Vec<PathBuf> = std::fs::read_dir(&tree.layout.python_releases)
        .expect("releases")
        .map(|entry| entry.expect("entry").path())
        .collect();
    let [python] = pythons.as_slice() else {
        panic!("one Python: {pythons:?}");
    };
    let manifest = verify::manifest_path(&tree.layout, python);
    let mode = std::fs::metadata(&manifest)
        .expect("written")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o644);
}

/// An untrusted manifest is said, and the Python isn't rebuilt for it.
#[tokio::test]
async fn an_untrusted_python_manifest_is_reported() {
    let _spawns = SPAWNS.lock().await;
    let (tree, active, _) = tree();
    let manifest = verify::manifest_path(&tree.layout, &active.python_install);
    verify::write_release_manifest(&active.python_install, &manifest, me()).expect("manifest");
    std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let (ran, outcome) = run(&tree, true).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(outcome, Some(VenvOutcome::UpToDate));
    assert!(
        ran.screen.contains("Python: not checked, ")
            && ran.screen.contains("writable by group or others"),
        "{}",
        ran.screen
    );
}

/// Two packages listing one file with different digests (the later
/// wheel's bytes on disk) are counted in the notice, not checked, and no
/// damage: the venv is kept.
#[tokio::test]
async fn a_file_two_packages_list_differently_is_counted_not_rebuilt() {
    let _spawns = SPAWNS.lock().await;
    let (tree, active, module) = tree();
    let site = module
        .parent()
        .and_then(Path::parent)
        .expect("site-packages");
    std::fs::create_dir_all(site.join("other-2.0.dist-info")).expect("dist-info");
    let sha = sb_release::sha256_reader(&b"VALUE = 9\n"[..]).expect("digest");
    let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha);
    std::fs::write(
        site.join("other-2.0.dist-info/RECORD"),
        format!("pkg/__init__.py,sha256={digest},10\n"),
    )
    .expect("record");
    let (ran, outcome) = run(&tree, true).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(outcome, Some(VenvOutcome::UpToDate));
    assert!(
        ran.screen.contains(
            "Packages: 0 files checked, 0 bad, 0 missing, 1 listed differently by two packages \
             (not checked)"
        ),
        "{}",
        ran.screen
    );
    assert!(active.venv.exists(), "the venv is kept");
}

/// A venv whose manifest names a Python outside sb's releases (a hand
/// edit) has its packages checked, and its Python not, with the reason;
/// the cleanup after then refuses it.
#[tokio::test]
async fn a_python_outside_the_releases_is_not_checked() {
    let _spawns = SPAWNS.lock().await;
    let (tree, active, _) = tree();
    let mut manifest = active.manifest.clone();
    let outside = tree
        .layout
        .python_releases
        .join("..")
        .join(active.python_install.file_name().expect("a name"));
    manifest.python_install = outside.display().to_string();
    tree.manifest(&active.dir, &manifest);
    let (ran, outcome) = run(&tree, true).await;
    assert_eq!(outcome, None);
    assert!(
        ran.error
            .as_deref()
            .is_some_and(|err| err.contains(&format!(
                "active Python installation {} is not directly under managed releases",
                outside.display()
            ))),
        "{:?}",
        ran.error
    );
    assert!(
        ran.screen.contains(&format!(
            "Python: not checked, {} is not a release sb manages",
            outside.display()
        )),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen
            .contains("Packages: 1 file checked, 0 bad, 0 missing"),
        "{}",
        ran.screen
    );
}
