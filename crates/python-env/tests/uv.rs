//! uv's install (python-env spec, step 2; module build M18): a wrong or
//! missing uv or uvx replaced by the pinned release from astral-sh,
//! through sb-release's fake GitHub, its archive holding the tree's
//! stand-ins. Nothing here touches `/usr/local/bin` or the network.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use sb_cli_core::cancel::Shutdown;
use sb_python_env::testing::{Tree, UV};
use sb_python_env::{Env, Force, reconcile_venv, uv};
use sb_release::Arch;
use sb_release::testing::Answer;
use sb_term_ui::{Labels, Options, Runner, Step, Target};
use sha2::Digest as _;
use tokio_util::sync::CancellationToken;

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise); the install writes and
/// runs its staged binaries.
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What a run left: its body's error chain, if any, and the screen.
struct Ran {
    error: Option<String>,
    screen: String,
}

/// `body` in a step of a runner drawing to memory; `cancel` signals
/// Ctrl-C first.
async fn in_step(cancel: bool, body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<()>) -> Ran {
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
    let _ = runner
        .step(Labels::new("Testing", "Tested"), async |step| {
            if cancel {
                shutdown.signalled(2);
            }
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

/// `env`'s uv install in a step.
async fn ensure(env: &Env<'_>) -> Ran {
    in_step(false, async |step| {
        uv::ensure_installed(step, env).await?;
        Ok(())
    })
    .await
}

/// The version `path` reports as `name`.
async fn version(env: &Env<'_>, path: &Path, name: &'static str) -> String {
    uv::version_of(env, path, name, &CancellationToken::new())
        .await
        .expect("probe")
}

/// The names in `dir` starting with `.` (staging leftovers).
fn hidden(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with('.'))
        .collect()
}

/// The paths the fake was asked for, without their queries.
fn asked(tree: &Tree) -> Vec<String> {
    tree.fake
        .seen()
        .into_iter()
        .map(|seen| seen.target.split('?').next().unwrap_or_default().to_owned())
        .collect()
}

/// A wrong uv is replaced on the normal path, inside "Ensuring uv <v> is
/// installed", and the reconcile goes on with the new one.
#[tokio::test]
async fn a_wrong_uv_is_replaced_by_the_pinned_release() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let _ = tree.healthy();
    tree.uv("0.13.3", "0.13.3");
    let arch = Arch::current().expect("arch");
    tree.serve_uv(arch, &tree.uv_archive(UV, UV));
    let env = tree.env();
    let ran = in_step(false, async |step| {
        reconcile_venv(step, &env, None, Force::None).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    for line in [
        "Ensuring uv 0.13.4 is installed",
        "Ansible virtual environment was already up to date",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert_eq!(version(&env, &tree.layout.uv, "uv").await, UV);
    assert_eq!(version(&env, &tree.layout.uvx, "uvx").await, UV);
    for path in [&tree.layout.uv, &tree.layout.uvx] {
        let mode = std::fs::metadata(path)
            .expect("binary")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "{}", path.display());
    }
    let name = uv::asset_name(arch);
    assert_eq!(
        asked(&tree),
        [
            "/version".to_owned(),
            format!("/api/repos/astral-sh/uv/releases/tags/{UV}"),
            format!("/astral-sh/uv/releases/download/{UV}/{name}"),
        ]
    );
    // Nothing staged is left beside the binaries or in the scratch.
    assert_eq!(hidden(&tree.layout.wrappers), Vec::<String>::new());
    let scratch: Vec<_> = std::fs::read_dir(&tree.layout.scratch)
        .expect("scratch")
        .collect();
    assert!(scratch.is_empty(), "{scratch:?}");
}

/// No uv and no uvx: both installed.
#[tokio::test]
async fn a_missing_uv_and_uvx_are_installed() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    std::fs::remove_file(&tree.layout.uv).expect("rm uv");
    std::fs::remove_file(&tree.layout.uvx).expect("rm uvx");
    tree.serve_uv(Arch::current().expect("arch"), &tree.uv_archive(UV, UV));
    let env = tree.env();
    let ran = ensure(&env).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(version(&env, &tree.layout.uv, "uv").await, UV);
    assert_eq!(version(&env, &tree.layout.uvx, "uvx").await, UV);
}

/// The pinned uv and uvx: nothing is asked of GitHub.
#[tokio::test]
async fn the_pinned_uv_asks_nothing() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let ran = ensure(&tree.env()).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(asked(&tree), Vec::<String>::new());
}

/// An arm64 `Env` asks for the aarch64 archive (decision 8), whatever
/// this build is: the archive's stand-ins are shell scripts.
#[tokio::test]
async fn an_arm64_env_downloads_the_aarch64_archive() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.uv("0.13.3", UV);
    tree.serve_uv(Arch::Arm64, &tree.uv_archive(UV, UV));
    let env = Env {
        arch: Arch::Arm64,
        ..tree.env()
    };
    let ran = ensure(&env).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        asked(&tree)
            .iter()
            .any(|path| path.ends_with("/uv-aarch64-unknown-linux-gnu.tar.gz")),
        "{:?}",
        asked(&tree)
    );
    assert_eq!(version(&env, &tree.layout.uv, "uv").await, UV);
}

/// A release without this architecture's archive: sb-go's wrapping of
/// release's text, nothing downloaded.
#[tokio::test]
async fn a_release_without_the_archive_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.uv("0.13.3", UV);
    let arch = Arch::current().expect("arch");
    let other = match arch {
        Arch::Amd64 => Arch::Arm64,
        Arch::Arm64 => Arch::Amd64,
    };
    tree.serve_uv(other, &tree.uv_archive(UV, UV));
    let ran = ensure(&tree.env()).await;
    assert_eq!(
        ran.error.as_deref(),
        Some(
            format!(
                "error fetching uv {UV} release metadata: release {UV} is missing {}",
                uv::asset_name(arch)
            )
            .as_str()
        )
    );
    assert_eq!(asked(&tree).len(), 2, "{:?}", asked(&tree));
}

/// The archive's bytes don't match the API's digest: nothing extracted,
/// the old uv kept.
#[tokio::test]
async fn a_digest_mismatch_installs_nothing() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.uv("0.13.3", UV);
    let archive = tree.uv_archive(UV, UV);
    let name = uv::asset_name(Arch::current().expect("arch"));
    let download = format!("/astral-sh/uv/releases/download/{UV}/{name}");
    let release = serde_json::json!({
        "tag_name": UV,
        "assets": [{
            "name": name,
            "size": archive.len(),
            "digest": format!("sha256:{}", hex::encode(sha2::Sha256::digest(&archive))),
            "browser_download_url": format!("{}{download}", tree.fake.url()),
        }],
    });
    tree.fake.script(
        &format!("/api/repos/astral-sh/uv/releases/tags/{UV}"),
        Answer::json(release.to_string()),
    );
    // The same size, other bytes.
    tree.fake
        .script(&download, Answer::new(200, vec![b'x'; archive.len()]));
    let env = tree.env();
    let ran = ensure(&env).await;
    assert_eq!(
        ran.error.as_deref(),
        Some(format!("error downloading uv: release asset {name:?} digest mismatch").as_str())
    );
    assert_eq!(version(&env, &tree.layout.uv, "uv").await, "0.13.3");
    assert_eq!(hidden(&tree.layout.wrappers), Vec::<String>::new());
}

/// A staged binary of another release is refused before anything is
/// activated (sb-go's text): the old uv and uvx stay.
#[tokio::test]
async fn a_staged_binary_of_another_release_is_refused() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.uv("0.13.2", "0.13.2");
    tree.serve_uv(
        Arch::current().expect("arch"),
        &tree.uv_archive(UV, "0.13.3"),
    );
    let env = tree.env();
    let ran = ensure(&env).await;
    assert_eq!(
        ran.error.as_deref(),
        Some("downloaded uvx reports version 0.13.3, expected 0.13.4")
    );
    assert_eq!(version(&env, &tree.layout.uv, "uv").await, "0.13.2");
    assert_eq!(version(&env, &tree.layout.uvx, "uvx").await, "0.13.2");
    assert_eq!(hidden(&tree.layout.wrappers), Vec::<String>::new());
}

/// Ctrl-C before the install: nothing is asked of GitHub, the old uv
/// stays.
#[tokio::test]
async fn a_cancel_installs_nothing() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.uv("0.13.3", UV);
    tree.serve_uv(Arch::current().expect("arch"), &tree.uv_archive(UV, UV));
    let env = tree.env();
    let ran = in_step(true, async |step| {
        uv::ensure_installed(step, &env).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error.as_deref(), Some("interrupted"));
    assert_eq!(asked(&tree), Vec::<String>::new());
    assert_eq!(version(&env, &tree.layout.uv, "uv").await, "0.13.3");
}

/// uv and uvx in different directories are refused before any request.
#[tokio::test]
async fn uv_and_uvx_must_share_a_directory() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let mut layout = tree.layout.clone();
    layout.uvx = tree.root.path().join("elsewhere/uvx");
    let env = sb_python_env::testing::with_stand_ins(
        Env::new(&layout, &tree.releases, UV, Arch::current().expect("arch")),
        tree.root.path(),
    );
    let ran = ensure(&env).await;
    assert_eq!(
        ran.error.as_deref(),
        Some("uv and uvx destinations must share a directory")
    );
    assert_eq!(asked(&tree), Vec::<String>::new());
}

/// uvx can't be replaced (here a directory holds its name): uv is the new
/// one, uvx's staged file is gone, nothing hidden is left beside them.
#[tokio::test]
async fn a_failed_activation_leaves_nothing_staged() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.uv("0.13.3", UV);
    std::fs::remove_file(&tree.layout.uvx).expect("rm uvx");
    std::fs::create_dir_all(tree.layout.uvx.join("in-the-way")).expect("uvx dir");
    tree.serve_uv(Arch::current().expect("arch"), &tree.uv_archive(UV, UV));
    let env = tree.env();
    let ran = ensure(&env).await;
    let error = ran.error.expect("failed");
    assert!(
        error.starts_with(&format!("error activating uvx {UV}: ")),
        "{error}"
    );
    assert_eq!(version(&env, &tree.layout.uv, "uv").await, UV);
    assert!(tree.layout.uvx.join("in-the-way").exists());
    assert_eq!(hidden(&tree.layout.wrappers), Vec::<String>::new());
}
