//! `reconcile_runtime` and `reconcile_venv` on a healthy stand-in tree,
//! and the generation cleanup (python-env spec, slice 1; sb-go's
//! `activation_test.go`). Nothing here touches `/srv`, `/usr/local/bin`,
//! apt or the network: every program is a stand-in on a temporary root.

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use sb_cli_core::cancel::Shutdown;
use sb_python_env::testing::{COMMANDS, Tree};
use sb_python_env::{Force, activate, reconcile_runtime, reconcile_venv};
use sb_term_ui::{Labels, Options, Runner, Step, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What a run left: its body's error chain, if any, and the screen.
struct Ran {
    error: Option<String>,
    screen: String,
}

/// `body` in a step of a runner drawing to memory.
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
        "Checking Ansible virtual environment and saltbox.fact for updates",
        "Ansible virtual environment and saltbox.fact are ready",
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

/// Whether the tests run as root (check.sh runs them so): ownership is
/// checked only then.
fn root() -> bool {
    sb_cli_core::root::effective_uid() == 0
}

#[tokio::test]
async fn a_healthy_tree_is_kept_with_sb_gos_steps() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let old = tree.generation(
        "20260101T000000.000000000-aaaaaaaaaaaa",
        "python-3-12-6-old",
    );
    std::os::unix::fs::symlink(&old.venv, tree.layout.ansible.join("previous")).expect("previous");
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_runtime(step, &env, None).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    for line in [
        "Checking for old deadsnakes Python packages",
        "Ensuring uv 0.13.4 is installed",
        "Ansible virtual environment was already up to date",
        "Ansible virtual environment ready",
        "saltbox.fact is ready",
        "Ansible virtual environment and saltbox.fact are ready",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    // The active generation and its Python stay; the old ones and the
    // obsolete link go.
    assert!(active.venv.join("bin/python3").exists());
    assert!(active.python_install.exists());
    assert!(!old.dir.exists());
    assert!(!old.python_install.exists());
    assert!(std::fs::symlink_metadata(tree.layout.ansible.join("previous")).is_err());
    assert_eq!(
        std::fs::read_link(&tree.layout.active_venv).expect("link"),
        active.venv
    );
    // A wrapper for each exported command, pointing at the active venv.
    for name in COMMANDS {
        let text = std::fs::read_to_string(tree.layout.wrappers.join(name)).expect("wrapper");
        let exec = format!(
            "exec {}/bin/{name} \"$@\"\n",
            tree.layout.active_venv.display()
        );
        assert!(text.ends_with(&exec), "{text}");
    }
    // libpq-dev is there: apt-get never ran.
    assert!(
        !tree
            .calls_made()
            .iter()
            .any(|call| call.contains("apt-get")),
        "{:?}",
        tree.calls_made()
    );
}

/// Rewriting a healthy tree's wrappers gives the same bytes (spec,
/// success criterion 1).
#[tokio::test]
async fn a_second_run_changes_nothing() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let _ = tree.healthy();
    let env = tree.env();
    let run = async || {
        let ran = in_step(async |step| {
            reconcile_runtime(step, &env, None).await?;
            Ok(())
        })
        .await;
        assert_eq!(ran.error, None, "{}", ran.screen);
    };
    run().await;
    let wrapper = tree.layout.wrappers.join("ansible");
    let first = std::fs::read(&wrapper).expect("wrapper");
    run().await;
    assert_eq!(std::fs::read(&wrapper).expect("wrapper"), first);
}

#[tokio::test]
async fn the_tree_is_given_to_the_saltbox_user() {
    if !root() {
        return;
    }
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, Some("nobody"), Force::None).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let uid = |path: &Path| std::fs::symlink_metadata(path).expect("stat").uid();
    // nobody's uid on Ubuntu (base-passwd).
    for path in [
        tree.layout.git_dir.clone(),
        tree.layout.lock.clone(),
        tree.layout.active_venv.clone(),
        active.venv.join("bin/python3"),
        active.python_install.clone(),
    ] {
        assert_eq!(uid(&path), 65534, "{}", path.display());
    }
}

#[tokio::test]
async fn libpq_dev_is_installed_when_missing() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let _ = tree.healthy();
    tree.libpq(false);
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, None, Force::None).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    for line in [
        "Updating apt package cache",
        "Installing native Python build prerequisites",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    let apt: Vec<String> = tree
        .calls_made()
        .into_iter()
        .filter(|call| call.contains("apt-get"))
        .collect();
    assert_eq!(apt.len(), 2, "{apt:?}");
    assert!(apt[0].contains(" update"), "{apt:?}");
    assert!(apt[1].ends_with("install -y libpq-dev"), "{apt:?}");
}

/// Each way a venv is unhealthy reads "update required"; the rebuild
/// itself comes with the build path.
#[tokio::test]
async fn an_unhealthy_venv_needs_an_update() {
    type Breaks = fn(&Tree, &sb_python_env::testing::Generation);
    let _spawns = SPAWNS.lock().await;
    let breaks: [(&str, Breaks); 5] = [
        ("lock", |tree, _| {
            std::fs::write(&tree.layout.lock, "changed\n").expect("lock");
        }),
        ("python pin", |tree, _| {
            std::fs::write(&tree.layout.python_pin, "3.13.1\n").expect("pin");
        }),
        ("schema 2", |tree, active| {
            let manifest = sb_python_env::manifest::Manifest {
                schema_version: 2,
                ..active.manifest.clone()
            };
            tree.manifest(&active.dir, &manifest);
        }),
        ("a probe", |_, active| {
            std::fs::remove_file(active.venv.join("bin/certbot")).expect("rm");
        }),
        ("missing", |tree, _| {
            std::fs::remove_file(&tree.layout.active_venv).expect("rm link");
        }),
    ];
    for (what, breaks) in breaks {
        let tree = Tree::new();
        let active = tree.healthy();
        breaks(&tree, &active);
        let env = tree.env();
        let ran = in_step(async |step| {
            reconcile_venv(step, &env, None, Force::None).await?;
            Ok(())
        })
        .await;
        assert!(
            ran.screen
                .contains("Ansible virtual environment update required"),
            "{what}:\n{}",
            ran.screen
        );
        assert!(
            ran.error
                .as_deref()
                .is_some_and(|e| e.contains("needs a rebuild")),
            "{what}: {:?}",
            ran.error
        );
    }
}

/// A real directory at the active path is the legacy layout: unhealthy,
/// left in place for activation to adopt.
#[tokio::test]
async fn a_legacy_directory_is_unhealthy() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.venv(&tree.layout.active_venv);
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, None, Force::None).await?;
        Ok(())
    })
    .await;
    assert!(ran.screen.contains("update required"), "{}", ran.screen);
    assert!(tree.layout.active_venv.join("bin/python3").exists());
}

#[tokio::test]
async fn a_forced_rebuild_says_so() {
    let _spawns = SPAWNS.lock().await;
    for (force, label) in [
        (
            Force::Venv,
            "Ansible virtual environment recreation requested",
        ),
        (
            Force::Python,
            "Python and Ansible virtual environment recreation requested",
        ),
    ] {
        let tree = Tree::new();
        let _ = tree.healthy();
        let env = tree.env();
        let ran = in_step(async |step| {
            reconcile_venv(step, &env, None, force).await?;
            Ok(())
        })
        .await;
        assert!(ran.screen.contains(label), "{label}:\n{}", ran.screen);
        assert!(
            ran.error
                .as_deref()
                .is_some_and(|e| e.contains("needs a rebuild")),
            "{label}: {:?}",
            ran.error
        );
        // Nothing of the healthy path ran: no wrapper, no cleanup.
        assert!(!tree.layout.wrappers.join("ansible").exists());
    }
}

/// The active venv as found: a dangling link and an incomplete manifest
/// are unhealthy; a file there, or a generation without its manifest,
/// fails the inspection with sb-go's words.
#[tokio::test]
async fn the_active_venv_is_inspected_as_sb_go_does() {
    let _spawns = SPAWNS.lock().await;
    let run = async |tree: &Tree| {
        let env = tree.env();
        in_step(async |step| {
            reconcile_venv(step, &env, None, Force::None).await?;
            Ok(())
        })
        .await
    };
    let tree = Tree::new();
    tree.activate(&tree.root.path().join("gone/venv"));
    let ran = run(&tree).await;
    assert!(ran.screen.contains("update required"), "{}", ran.screen);

    let tree = Tree::new();
    let active = tree.healthy();
    let mut value: serde_json::Value = serde_json::to_value(&active.manifest).expect("json");
    value
        .as_object_mut()
        .expect("object")
        .remove("saltbox_commit");
    std::fs::write(active.dir.join("manifest.json"), value.to_string()).expect("manifest");
    let ran = run(&tree).await;
    assert!(ran.screen.contains("update required"), "{}", ran.screen);

    let tree = Tree::new();
    std::fs::create_dir_all(&tree.layout.ansible).expect("ansible");
    std::fs::write(&tree.layout.active_venv, "a file").expect("file");
    let ran = run(&tree).await;
    let want = format!(
        "inspect active Ansible environment: {} is neither a directory nor a symlink",
        tree.layout.active_venv.display()
    );
    assert_eq!(ran.error.as_deref(), Some(want.as_str()));

    let tree = Tree::new();
    let active = tree.healthy();
    std::fs::remove_file(active.dir.join("manifest.json")).expect("rm");
    let ran = run(&tree).await;
    let error = ran.error.expect("no manifest");
    assert!(
        error.starts_with("inspect active Ansible environment: read generation manifest "),
        "{error}"
    );
}

/// A failure's cause shows once in its block, not again as a source.
#[tokio::test]
async fn a_failures_cause_is_shown_once() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let manifest = sb_python_env::manifest::Manifest {
        python_install: tree
            .root
            .path()
            .join("external-python")
            .display()
            .to_string(),
        ..active.manifest.clone()
    };
    tree.manifest(&active.dir, &manifest);
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, None, Force::None).await?;
        Ok(())
    })
    .await;
    // The chain as the failure block and the run log get it.
    let error = ran.error.expect("outside");
    assert!(
        error.starts_with("clean old Ansible generations: active Python installation "),
        "{error}"
    );
    assert_eq!(error.matches("external-python").count(), 1, "{error}");
}

#[tokio::test]
async fn a_wrong_uv_fails_its_step() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let _ = tree.healthy();
    tree.uv("0.13.3", "0.13.4");
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_runtime(step, &env, None).await?;
        Ok(())
    })
    .await;
    assert!(ran.screen.contains("install pinned uv"), "{}", ran.screen);
    // The nested step shows the cause; the steps above it end quietly.
    assert!(
        ran.screen
            .contains("reports version 0.13.3, expected 0.13.4"),
        "{}",
        ran.screen
    );
    assert!(ran.error.is_some());
}

/// sb-go's `TestCleanupGenerationsFailsClosedWithoutActiveGeneration`.
#[tokio::test]
async fn cleanup_without_an_active_generation_removes_nothing() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let old = tree.generation("old", "python-old");
    assert!(activate::cleanup_generations(&tree.layout).is_err());
    assert!(old.dir.exists());
    assert!(old.python_install.exists());
}

/// sb-go's `TestCleanupGenerationsRejectsActiveTargetOutsideManagedReleases`.
#[tokio::test]
async fn cleanup_refuses_an_active_venv_outside_releases() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let old = tree.generation("old", "python-old");
    let outside = tree.root.path().join("elsewhere/venv");
    std::fs::create_dir_all(&outside).expect("outside");
    tree.activate(&outside);
    let err = activate::cleanup_generations(&tree.layout).expect_err("outside");
    assert!(err.to_string().starts_with("active generation "), "{err}");
    assert!(old.dir.exists());
    assert!(old.python_install.exists());
}

/// sb-go's `TestCleanupGenerationsRejectsPythonOutsideManagedReleases`.
#[tokio::test]
async fn cleanup_refuses_a_python_outside_releases() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let old = tree.generation("old", "python-old");
    let manifest = sb_python_env::manifest::Manifest {
        python_install: tree
            .root
            .path()
            .join("external-python")
            .display()
            .to_string(),
        ..active.manifest.clone()
    };
    tree.manifest(&active.dir, &manifest);
    let err = activate::cleanup_generations(&tree.layout).expect_err("outside");
    assert!(
        err.to_string().starts_with("active Python installation "),
        "{err}"
    );
    for kept in [
        &active.dir,
        &old.dir,
        &active.python_install,
        &old.python_install,
    ] {
        assert!(kept.exists(), "{}", kept.display());
    }
}
