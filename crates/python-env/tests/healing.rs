//! Self-healing on the normal path (python-env spec, "Self-healing on the
//! normal path"; decision 3; module build M17): the wider Python check
//! finds a broken venv, and a failed non-forced build is retried once,
//! cleanly, as a `Python` force. Through `reconcile_runtime`, which `sb
//! update`, `sb update-venv` and `sb branch` call. Nothing here touches
//! `/srv`, `/usr/local/bin`, apt or the network: every program is a
//! stand-in on a temporary root.

use std::path::{Path, PathBuf};

use sb_cli_core::cancel::Shutdown;
use sb_python_env::testing::{PYTHON, Tree, stand_in};
use sb_python_env::{Force, VenvOutcome, reconcile_runtime, reconcile_venv};
use sb_term_ui::{Labels, Options, Runner, Step, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What a run left: its body's error chain, if any, and the screen.
struct Ran {
    error: Option<String>,
    screen: String,
}

/// `body` in a step of a runner drawing to memory; `body` gets the
/// shutdown too, to signal Ctrl-C.
async fn in_step(body: impl AsyncFnOnce(&mut Step, &Shutdown) -> anyhow::Result<()>) -> Ran {
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
        "Checking Ansible virtual environment and saltbox.fact for updates",
        "Ansible virtual environment and saltbox.fact are ready",
    );
    let _ = runner
        .step(labels, async |step| {
            let result = body(step, &shutdown).await;
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

/// The uv calls made, without the uv path and `--no-config`.
fn uv_calls(tree: &Tree) -> Vec<String> {
    let uv = format!("{} --no-config ", tree.layout.uv.display());
    tree.calls_made()
        .into_iter()
        .filter_map(|call| call.strip_prefix(&uv).map(str::to_string))
        .collect()
}

/// The `uv pip sync` calls made.
fn syncs(tree: &Tree) -> Vec<String> {
    uv_calls(tree)
        .into_iter()
        .filter(|call| call.starts_with("pip sync"))
        .collect()
}

/// The generation the active link names.
fn active_generation(tree: &Tree) -> PathBuf {
    let venv = std::fs::read_link(&tree.layout.active_venv).expect("link");
    venv.parent().expect("generation").to_path_buf()
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

/// The healthy tree made to need a rebuild: its lock changed.
fn outdated_tree() -> (Tree, sb_python_env::testing::Generation) {
    let tree = Tree::new();
    let active = tree.healthy();
    std::fs::write(&tree.layout.lock, "ansible==12.1.0 --hash=sha256:01\n").expect("lock");
    (tree, active)
}

const RETRYING: &str = "Retrying with a fresh Python and no uv cache after: ";

/// A venv whose Python fails the wider check (`ssl` doesn't import; sb-go
/// imports `encodings` only) is unhealthy and rebuilt, once: its Python
/// release, which passes, is reused.
#[tokio::test]
async fn a_venv_failing_the_wider_check_is_rebuilt_once() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let script = format!(
        "[ \"$1\" = -I ] || exit 2\ncase \"$2\" in\n--version) echo 'Python {PYTHON}' ;;\n\
         -c) case \"$3\" in *ssl*) echo 'No module named _ssl' >&2; exit 1 ;; esac ;;\nesac"
    );
    let _ = stand_in(&active.venv.join("bin"), "python3", &script);
    let env = tree.env();
    let mut outcome = None;
    let ran = in_step(async |step, _| {
        outcome = Some(reconcile_venv(step, &env, None, Force::None).await?);
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(outcome, Some(VenvOutcome::Rebuilt));
    assert!(ran.screen.contains("update required"), "{}", ran.screen);
    assert!(!ran.screen.contains(RETRYING), "{}", ran.screen);
    assert_eq!(syncs(&tree).len(), 1, "{:?}", uv_calls(&tree));
    // A normal rebuild keeps uv's cache.
    assert!(
        !uv_calls(&tree)
            .iter()
            .any(|call| call.starts_with("cache clean")),
        "{:?}",
        uv_calls(&tree)
    );
    assert_ne!(active_generation(&tree), active.dir);
}

/// `pip sync` fails once: the partial build goes, and the build runs once
/// more as a `Python` force, a fresh Python release and both uv calls
/// without the cache; the info line names the first cause.
#[tokio::test]
async fn a_failed_build_is_retried_once_cleanly() {
    let _spawns = SPAWNS.lock().await;
    let (tree, active) = outdated_tree();
    tree.fail_once("pip-sync");
    let env = tree.env();
    let ran = in_step(async |step, _| {
        reconcile_runtime(step, &env, None).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let line = format!("{RETRYING}error syncing requirements: ");
    assert!(ran.screen.contains(&line), "{}", ran.screen);
    assert!(
        ran.screen.contains("Ansible virtual environment ready"),
        "{}",
        ran.screen
    );
    let syncs = syncs(&tree);
    assert_eq!(syncs.len(), 2, "{syncs:?}");
    assert!(!syncs[0].contains("--no-cache"), "{syncs:?}");
    assert!(syncs[1].contains(" --no-cache "), "{syncs:?}");
    // uv's cache is cleared once, between the two builds: a damaged
    // cache entry can't come back in the next normal build.
    let calls = uv_calls(&tree);
    let at = |prefix: &str| {
        calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.starts_with(prefix))
            .map(|(at, _)| at)
            .collect::<Vec<_>>()
    };
    let (cleans, sync_calls) = (at("cache clean"), at("pip sync"));
    assert_eq!(cleans.len(), 1, "{calls:?}");
    assert!(
        sync_calls[0] < cleans[0] && cleans[0] < sync_calls[1],
        "{calls:?}"
    );
    let installs: Vec<String> = uv_calls(&tree)
        .into_iter()
        .filter(|call| call.starts_with("python install"))
        .collect();
    // The first build reused the active Python; the retry installs afresh.
    assert_eq!(installs.len(), 1, "{installs:?}");
    assert!(
        installs[0].contains(" --reinstall --no-cache "),
        "{installs:?}"
    );
    // One generation and one Python release are left: the retry's.
    let generation = active_generation(&tree);
    let id = generation.file_name().expect("id").to_string_lossy();
    assert_eq!(entries(&tree.layout.ansible_releases), [id.as_ref()]);
    let pythons = entries(&tree.layout.python_releases);
    assert_eq!(pythons.len(), 1, "{pythons:?}");
    assert!(!active.python_install.exists());
}

/// The retry's outcome says it retried.
#[tokio::test]
async fn a_retried_build_says_so() {
    let _spawns = SPAWNS.lock().await;
    let (tree, _) = outdated_tree();
    tree.fail_once("venv---python");
    let env = tree.env();
    let mut outcome = None;
    let ran = in_step(async |step, _| {
        outcome = Some(reconcile_venv(step, &env, None, Force::None).await?);
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(outcome, Some(VenvOutcome::Retried));
    assert!(
        ran.screen
            .contains(&format!("{RETRYING}error creating venv: ")),
        "{}",
        ran.screen
    );
}

/// A failure the staged check finds, outside a nested step, is retried
/// too, its own text as the cause (the stand-in `uv pip check` fails for
/// good here, so the retry fails as well).
#[tokio::test]
async fn a_failed_staged_check_is_retried() {
    let _spawns = SPAWNS.lock().await;
    let (tree, _) = outdated_tree();
    tree.fail("pip-check");
    let env = tree.env();
    let ran = in_step(async |step, _| {
        reconcile_venv(step, &env, None, Force::None).await?;
        Ok(())
    })
    .await;
    let line = format!(
        "{RETRYING}validate staged Ansible environment: installed Python packages are incompatible: "
    );
    assert!(ran.screen.contains(&line), "{}", ran.screen);
    assert_eq!(syncs(&tree).len(), 2, "{:?}", uv_calls(&tree));
    let error = ran.error.expect("failed twice");
    assert!(
        error.starts_with("validate staged Ansible environment: "),
        "{error}"
    );
}

/// A second failure is reported, not retried: the first build fails at
/// `uv venv`, the retry at `pip sync`; the step fails with the second
/// cause, and the previous generation stays active.
#[tokio::test]
async fn a_second_failure_is_reported() {
    let _spawns = SPAWNS.lock().await;
    let (tree, active) = outdated_tree();
    tree.fail_once("venv---python");
    tree.fail("pip-sync");
    let env = tree.env();
    let ran = in_step(async |step, _| {
        reconcile_runtime(step, &env, None).await?;
        Ok(())
    })
    .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert_eq!(ran.screen.matches(RETRYING).count(), 1, "{}", ran.screen);
    let first = format!("{RETRYING}error creating venv: ");
    assert!(ran.screen.contains(&first), "{}", ran.screen);
    // The second cause, under the retry's failed step.
    assert!(
        ran.screen.contains("✗ Syncing hashed Saltbox requirements"),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen.contains("  error syncing requirements"),
        "{}",
        ran.screen
    );
    let syncs = syncs(&tree);
    assert_eq!(syncs.len(), 1, "{syncs:?}");
    assert!(syncs[0].contains(" --no-cache "), "{syncs:?}");
    assert_eq!(active_generation(&tree), active.dir);
    assert_eq!(
        entries(&tree.layout.ansible_releases),
        [sb_python_env::testing::GENERATION]
    );
    assert_eq!(
        entries(&tree.layout.python_releases),
        [sb_python_env::testing::PYTHON_RELEASE]
    );
}

/// A forced rebuild that fails isn't retried: the user asked for exactly
/// that build.
#[tokio::test]
async fn a_forced_build_is_not_retried() {
    let _spawns = SPAWNS.lock().await;
    for force in [Force::Venv, Force::Python] {
        let tree = Tree::new();
        let _ = tree.healthy();
        tree.fail("pip-sync");
        let env = tree.env();
        let ran = in_step(async |step, _| {
            reconcile_venv(step, &env, None, force).await?;
            Ok(())
        })
        .await;
        assert!(ran.error.is_some(), "{force:?}");
        assert!(!ran.screen.contains(RETRYING), "{force:?}:\n{}", ran.screen);
        assert_eq!(syncs(&tree).len(), 1, "{force:?}");
    }
}

/// Wait for `path`, up to 20 s.
async fn appears(path: &Path) {
    for _ in 0..400 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{} never appeared", path.display());
}

/// Ctrl-C during the build isn't retried.
#[tokio::test]
async fn a_cancelled_build_is_not_retried() {
    let _spawns = SPAWNS.lock().await;
    let (tree, active) = outdated_tree();
    tree.hang("pip-sync");
    let env = tree.env();
    let started = tree.root.path().join("hang/started");
    let ran = in_step(async |step, shutdown| {
        let cancel = async {
            appears(&started).await;
            shutdown.signalled(2);
        };
        let (result, ()) = tokio::join!(reconcile_venv(step, &env, None, Force::None), cancel);
        result?;
        Ok(())
    })
    .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(!ran.screen.contains(RETRYING), "{}", ran.screen);
    assert_eq!(syncs(&tree).len(), 1, "{:?}", uv_calls(&tree));
    assert_eq!(active_generation(&tree), active.dir);
}

/// A failure before the build (a wrong uv) isn't retried.
#[tokio::test]
async fn a_uv_failure_is_not_retried() {
    let _spawns = SPAWNS.lock().await;
    let (tree, _) = outdated_tree();
    tree.uv("0.13.3", "0.13.4");
    let env = tree.env();
    let ran = in_step(async |step, _| {
        reconcile_venv(step, &env, None, Force::None).await?;
        Ok(())
    })
    .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(!ran.screen.contains(RETRYING), "{}", ran.screen);
    assert_eq!(syncs(&tree), Vec::<String>::new());
}
