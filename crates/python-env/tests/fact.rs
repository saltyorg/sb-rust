//! saltbox.fact (python-env spec, "saltbox.fact"; module build M19): the
//! latest release of saltyorg/ansible-facts from sb-release's fake GitHub,
//! the installed fact's `--version` against it with the digest rule, and
//! the install with its checks. Nothing here touches the network or a real
//! Saltbox checkout.

use std::os::unix::fs::PermissionsExt as _;

use sb_cli_core::cancel::Shutdown;
use sb_python_env::fact::{self, FactMode, FactOutcome};
use sb_python_env::testing::{FACT_TAG, Tree, fact_binary};
use sb_python_env::{Env, reconcile_runtime};
use sb_release::Arch;
use sb_term_ui::{Labels, Options, Runner, Step, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise); the install writes and
/// runs its staged fact.
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What a run left: its body's error chain, the outcome and the screen.
struct Ran {
    error: Option<String>,
    outcome: Option<FactOutcome>,
    screen: String,
}

/// `install_fact(mode)` in a step of a runner drawing to memory.
async fn install(env: &Env<'_>, mode: FactMode) -> Ran {
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
    let mut outcome = None;
    let _ = runner
        .step(
            Labels::new("Checking saltbox.fact", "saltbox.fact is ready"),
            async |step: &mut Step| {
                let result = fact::install_fact(step, env, mode).await;
                match result {
                    Ok(done) => {
                        outcome = Some(done);
                        Ok(())
                    }
                    Err(err) => {
                        let err = anyhow::Error::from(err);
                        error = Some(format!("{err:#}"));
                        Err(err)
                    }
                }
            },
        )
        .await;
    runner.finish();
    Ran {
        error,
        outcome,
        screen: term.contents(),
    }
}

/// The paths the fake was asked for, without their queries.
fn asked(tree: &Tree) -> Vec<String> {
    tree.fake
        .seen()
        .into_iter()
        .map(|seen| seen.target.split('?').next().unwrap_or_default().to_owned())
        .collect()
}

fn arch() -> Arch {
    Arch::current().expect("arch")
}

fn other(arch: Arch) -> Arch {
    match arch {
        Arch::Amd64 => Arch::Arm64,
        Arch::Arm64 => Arch::Amd64,
    }
}

/// The release's asset path on the fake.
fn download(tag: &str, arch: Arch) -> String {
    format!(
        "/saltyorg/ansible-facts/releases/download/{tag}/{}",
        fact::asset_name(arch)
    )
}

const LATEST: &str = "/api/repos/saltyorg/ansible-facts/releases/latest";

/// A fact with the release's bytes at its version is kept, with sb-go's
/// line, after one metadata request (through GitHub: the proxy is down).
#[tokio::test]
async fn the_releases_fact_is_up_to_date() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    tree.fact(&body);
    let ran = install(&tree.env(), FactMode::Update).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        ran.outcome,
        Some(FactOutcome::UpToDate {
            version: "1.4.0".to_owned()
        })
    );
    for line in [
        "Fetching latest saltbox.fact release info",
        "saltbox.fact is up to date (version 1.4.0)",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert_eq!(asked(&tree), ["/version", LATEST]);
}

/// The released version with other bytes is replaced (decision 2), with
/// the download as the only further request (success criterion 6).
#[tokio::test]
async fn a_tampered_fact_at_the_released_version_is_reinstalled() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    let mut tampered = body.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    tree.fact(&tampered);
    let ran = install(&tree.env(), FactMode::Update).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(
        ran.outcome,
        Some(FactOutcome::Repaired {
            version: FACT_TAG.to_owned()
        })
    );
    for line in [
        "saltbox.fact 1.4.0 does not match its release (sha256 ",
        "Reinstalling saltbox.fact with version v1.4.0",
        "Validating downloaded saltbox.fact",
        "saltbox.fact reinstalled successfully (version v1.4.0)",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), body);
    assert_eq!(
        asked(&tree),
        ["/version", LATEST, &download(FACT_TAG, arch())]
    );
}

/// An older fact is updated, with sb-go's line naming both versions; the
/// new one is 0755 and nothing staged is left.
#[tokio::test]
async fn an_older_fact_is_updated() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    tree.fact_script("echo v1.3.9");
    let ran = install(&tree.env(), FactMode::Update).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        ran.screen
            .contains("Updating saltbox.fact to version v1.4.0"),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen
            .contains("saltbox.fact updated successfully: v1.3.9 → v1.4.0"),
        "{}",
        ran.screen
    );
    assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), body);
    let mode = std::fs::metadata(&tree.layout.fact)
        .expect("fact")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    let dir = tree.layout.fact.parent().expect("dir");
    let names: Vec<_> = std::fs::read_dir(dir)
        .expect("dir")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(names, ["saltbox.fact"]);
}

/// A newer local fact is kept (decision 2).
#[tokio::test]
async fn a_newer_fact_is_kept() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.serve_fact(arch(), FACT_TAG, &fact_binary("1.4.0"));
    tree.fact_script("echo 1.5.0");
    let ran = install(&tree.env(), FactMode::Update).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        ran.screen
            .contains("saltbox.fact is up to date (version 1.5.0)"),
        "{}",
        ran.screen
    );
    assert_eq!(asked(&tree).len(), 2);
}

/// No fact: installed, with sb-go's lines.
#[tokio::test]
async fn a_missing_fact_is_installed() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    std::fs::create_dir_all(tree.layout.fact.parent().expect("dir")).expect("dir");
    let ran = install(&tree.env(), FactMode::Update).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    for line in [
        "saltbox.fact not found. Proceeding with update.",
        "saltbox.fact installed successfully (version v1.4.0)",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), body);
}

/// A checkout without `ansible_facts.d` (Saltbox tracks none, it holds
/// only the ignored fact): the directory is made, 0755, as sb-go's
/// `DownloadVerified` makes it, and the fact installed (found by the live
/// acceptance, module build M21).
#[tokio::test]
async fn a_missing_facts_directory_is_created() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    let dir = tree.layout.fact.parent().expect("dir").to_owned();
    assert!(!dir.exists(), "the tree has no facts directory");
    let ran = install(&tree.env(), FactMode::Update).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), body);
    // DirBuilder's mode passes through the umask, as Go's MkdirAll does:
    // check.sh runs under root's usual 022.
    let mode = std::fs::metadata(&dir).expect("dir").permissions().mode();
    assert_eq!(mode & 0o7777, 0o755);
}

/// A facts directory that can't be made (a dangling link in its place:
/// the fact reads as missing, the directory can't be made) is named with
/// sb-go's text, and nothing is downloaded.
#[tokio::test]
async fn a_facts_directory_that_cant_be_made_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    let dir = tree.layout.fact.parent().expect("dir");
    std::fs::create_dir_all(dir.parent().expect("checkout")).expect("checkout");
    std::os::unix::fs::symlink(dir.with_extension("gone"), dir).expect("link");
    let ran = install(&tree.env(), FactMode::Update).await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    // The install step's error block: each context on its line, then
    // the cause.
    for line in [
        "error downloading saltbox.fact",
        "create release asset staging directory",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert!(!asked(&tree).contains(&download(FACT_TAG, arch())));
}

/// A fact whose `--version` fails, or prints nothing it can read, is
/// replaced with sb-go's warning; it is never run bare.
#[tokio::test]
async fn an_unreadable_fact_version_is_replaced() {
    for (script, warning) in [
        ("exit 3", "failed to run saltbox.fact: "),
        (
            "echo",
            "fact version output is empty. Proceeding with update.",
        ),
        (
            "echo 1.4.0; echo 1.4.1",
            "plain fact version output must be one line. Proceeding with update.",
        ),
    ] {
        let _spawns = SPAWNS.lock().await;
        let tree = Tree::new();
        let body = fact_binary("1.4.0");
        tree.serve_fact(arch(), FACT_TAG, &body);
        tree.fact_script(script);
        let ran = install(&tree.env(), FactMode::Update).await;
        assert_eq!(ran.error, None, "{script}: {}", ran.screen);
        assert!(ran.screen.contains(warning), "{script}:\n{}", ran.screen);
        assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), body);
        let calls = tree.calls_made();
        let fact = tree.layout.fact.display().to_string();
        assert_eq!(calls, [format!("{fact} --version")], "{script}");
    }
}

/// Reinstall replaces even the release's own bytes, with sb-go's lines.
#[tokio::test]
async fn reinstall_replaces_an_up_to_date_fact() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    tree.fact(&body);
    let ran = install(&tree.env(), FactMode::Reinstall).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    for line in [
        "Reinstall forced.",
        "Reinstalling saltbox.fact with version v1.4.0",
        "saltbox.fact reinstalled successfully (version v1.4.0)",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert_eq!(asked(&tree).len(), 3, "{:?}", asked(&tree));
}

/// An arm64 `Env` asks for the arm64 asset only, and refuses this
/// build's x86-64 fact served under that name (success criterion 7). The
/// existing fact stays.
#[tokio::test]
async fn another_architectures_fact_is_refused() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let wanted = other(arch());
    tree.serve_fact(wanted, FACT_TAG, &fact_binary("1.4.0"));
    tree.fact_script("echo 1.3.0");
    let before = std::fs::read(&tree.layout.fact).expect("fact");
    let env = Env {
        arch: wanted,
        ..tree.env()
    };
    let ran = install(&env, FactMode::Update).await;
    let machine = match wanted {
        Arch::Amd64 => "x86-64",
        Arch::Arm64 => "aarch64",
    };
    for line in [
        "downloaded binary validation failed".to_owned(),
        format!("file is not an {machine} ELF binary"),
    ] {
        assert!(ran.screen.contains(&line), "{line}:\n{}", ran.screen);
    }
    assert!(ran.error.is_some());
    assert_eq!(
        asked(&tree),
        ["/version", LATEST, &download(FACT_TAG, wanted)]
    );
    assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), before);
}

/// A staged fact reporting another version than its tag is refused
/// (decision 20).
#[tokio::test]
async fn a_staged_fact_of_another_version_is_refused() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.serve_fact(arch(), FACT_TAG, &fact_binary("1.3.0"));
    std::fs::create_dir_all(tree.layout.fact.parent().expect("dir")).expect("dir");
    let ran = install(&tree.env(), FactMode::Update).await;
    for line in [
        "downloaded binary validation failed",
        "downloaded saltbox.fact reports version 1.3.0, expected v1.4.0",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    // The staged file is gone with the failure.
    let dir = tree.layout.fact.parent().expect("dir");
    assert_eq!(std::fs::read_dir(dir).expect("dir").count(), 0);
    assert!(!tree.layout.fact.exists());
}

/// A release without this architecture's asset: release's text in the
/// metadata step.
#[tokio::test]
async fn a_release_without_the_asset_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.serve_fact(other(arch()), FACT_TAG, &fact_binary("1.4.0"));
    let ran = install(&tree.env(), FactMode::Update).await;
    assert!(
        ran.screen.contains(&format!(
            "release v1.4.0 is missing {}",
            fact::asset_name(arch())
        )),
        "{}",
        ran.screen
    );
    assert!(ran.error.is_some());
}

/// `reconcile_runtime`'s fact step is the real one now: a tampered fact
/// on a healthy tree is repaired by the normal path.
#[tokio::test]
async fn the_runtime_reconcile_repairs_the_fact() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let _ = tree.healthy();
    tree.fact_script("echo 1.4.0");
    let env = tree.env();
    let term = indicatif::InMemoryTerm::new(80, 400);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb test", &shutdown, None, options);
    let result = runner
        .step(Labels::new("Checking", "Checked"), async |step| {
            reconcile_runtime(step, &env, None).await?;
            Ok(())
        })
        .await;
    runner.finish();
    let screen = term.contents();
    assert!(result.is_ok(), "{screen}");
    assert!(
        screen.contains("saltbox.fact reinstalled successfully (version v1.4.0)"),
        "{screen}"
    );
    assert_eq!(
        std::fs::read(&tree.layout.fact).expect("fact"),
        fact_binary("1.4.0")
    );
}

/// A fact that can't be looked at (its directory is a file) stops the
/// check with sb-go's text, before any download.
#[tokio::test]
async fn a_fact_that_cant_be_looked_at_is_an_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.serve_fact(arch(), FACT_TAG, &fact_binary("1.4.0"));
    let mut layout = tree.layout.clone();
    let file = tree.root.path().join("a-file");
    std::fs::write(&file, "").expect("file");
    layout.fact = file.join("saltbox.fact");
    let env = Env {
        layout: &layout,
        ..tree.env()
    };
    let ran = install(&env, FactMode::Update).await;
    let error = ran.error.expect("failed");
    assert!(
        error.starts_with("error checking for existing saltbox.fact: "),
        "{error}"
    );
    assert_eq!(asked(&tree).len(), 2, "{:?}", asked(&tree));
}

/// A legacy fact (JSON with `saltbox_facts_version`) at the released
/// version is another file than the release's: reinstalled.
#[tokio::test]
async fn a_legacy_fact_at_the_released_version_is_reinstalled() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    tree.fact_script(r#"echo '{"saltbox_facts_version": "1.4.0"}'"#);
    let ran = install(&tree.env(), FactMode::Update).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        ran.screen
            .contains("saltbox.fact 1.4.0 does not match its release"),
        "{}",
        ran.screen
    );
    assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), body);
}

/// A legacy fact whose version isn't semver: sb-go's warning, updated.
#[tokio::test]
async fn an_unparseable_current_version_is_updated() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let body = fact_binary("1.4.0");
    tree.serve_fact(arch(), FACT_TAG, &body);
    tree.fact_script(r#"echo '{"saltbox_facts_version": "1.0"}'"#);
    let ran = install(&tree.env(), FactMode::Update).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    for line in [
        "Failed to parse current version: ",
        "saltbox.fact updated successfully: 1.0 → v1.4.0",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), body);
}

/// A latest tag that isn't semver: sb-go's warning, then the staged
/// fact's version refused, as it isn't the tag.
#[tokio::test]
async fn an_unparseable_latest_tag_is_compared_as_text() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.serve_fact(arch(), "latest", &fact_binary("1.4.0"));
    tree.fact_script("echo 1.3.0");
    let ran = install(&tree.env(), FactMode::Update).await;
    for line in [
        "Failed to parse latest version: ",
        "Updating saltbox.fact to version latest",
        "downloaded saltbox.fact reports version 1.4.0, expected latest",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert!(ran.error.is_some());
}
