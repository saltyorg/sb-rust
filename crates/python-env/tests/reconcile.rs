//! `reconcile_runtime` and `reconcile_venv` on a healthy stand-in tree,
//! and the generation cleanup (python-env spec, slice 1; sb-go's
//! `activation_test.go`). Nothing here touches `/srv`, `/usr/local/bin`,
//! apt or the network: every program is a stand-in on a temporary root.

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use sb_cli_core::cancel::Shutdown;
use sb_python_env::testing::{COMMANDS, Tree};
use sb_python_env::{Force, VenvOutcome, activate, reconcile_runtime, reconcile_venv};
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
    in_step_with(async |step, _| body(step).await).await
}

/// [`in_step`], `body` getting the shutdown too, to signal Ctrl-C.
async fn in_step_with(body: impl AsyncFnOnce(&mut Step, &Shutdown) -> anyhow::Result<()>) -> Ran {
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
    // The healthy path cleans without a step of its own (spec, step 6).
    assert!(!ran.screen.contains("old environments"), "{}", ran.screen);
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
    // The healthy path gives ownership without a step (spec, step 6).
    assert!(!ran.screen.contains("Setting ownership"), "{}", ran.screen);
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

/// The generation the active link names.
fn active_generation(tree: &Tree) -> std::path::PathBuf {
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

/// The uv calls made, without the uv path and `--no-config`.
fn uv_calls(tree: &Tree) -> Vec<String> {
    let uv = format!("{} --no-config ", tree.layout.uv.display());
    tree.calls_made()
        .into_iter()
        .filter_map(|call| call.strip_prefix(&uv).map(str::to_string))
        .collect()
}

/// Each way a venv is unhealthy reads "update required" and is rebuilt:
/// a new generation, active, the old one cleaned away.
#[tokio::test]
async fn an_unhealthy_venv_is_rebuilt() {
    type Breaks = fn(&Tree, &sb_python_env::testing::Generation);
    let _spawns = SPAWNS.lock().await;
    let breaks: [(&str, Breaks); 4] = [
        ("lock", |tree, _| {
            std::fs::write(&tree.layout.lock, "changed\n").expect("lock");
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
            let outcome = reconcile_venv(step, &env, None, Force::None).await?;
            assert_eq!(outcome, VenvOutcome::Rebuilt);
            Ok(())
        })
        .await;
        assert_eq!(ran.error, None, "{what}:\n{}", ran.screen);
        assert!(
            ran.screen
                .contains("Ansible virtual environment update required"),
            "{what}:\n{}",
            ran.screen
        );
        let generation = active_generation(&tree);
        assert_ne!(generation, active.dir, "{what}");
        let id = generation.file_name().expect("id").to_string_lossy();
        assert_eq!(
            entries(&tree.layout.ansible_releases),
            [id.as_ref()],
            "{what}"
        );
        let written = sb_python_env::manifest::read(&generation)
            .expect("manifest")
            .expect("complete");
        let lock = sb_python_env::toolchain::file_sha256(&tree.layout.lock).expect("lock");
        assert_eq!(written.lock_sha256, lock, "{what}");
        for name in &written.exported_commands {
            assert!(tree.layout.wrappers.join(name).exists(), "{what}: {name}");
        }
    }
}

/// A changed `.python-version` needs a new Python release: the active
/// one isn't reused, and the new generation runs on the new Python.
#[tokio::test]
async fn a_changed_python_pin_installs_that_python() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    std::fs::write(&tree.layout.python_pin, "3.13.1\n").expect("pin");
    // What the stand-in uv installs and puts into the venv.
    tree.python(&tree.root.path().join("templates/python3"), "3.13.1");
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, None, Force::None).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(ran.screen.contains("update required"), "{}", ran.screen);
    assert!(
        ran.screen.contains("Installing Python 3.13.1"),
        "{}",
        ran.screen
    );
    let written = sb_python_env::manifest::read(&active_generation(&tree))
        .expect("manifest")
        .expect("complete");
    assert_eq!(written.python_version, "3.13.1");
    assert!(!active.python_install.exists(), "the old Python is cleaned");
}

/// A forced rebuild of a healthy venv says so and builds: `Venv` on the
/// active Python, `Python` on a new release with `--reinstall`; both
/// without uv's cache.
#[tokio::test]
async fn a_forced_rebuild_says_so_and_builds() {
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
        let active = tree.healthy();
        let env = tree.env();
        let ran = in_step(async |step| {
            reconcile_venv(step, &env, None, force).await?;
            Ok(())
        })
        .await;
        assert_eq!(ran.error, None, "{label}:\n{}", ran.screen);
        assert!(ran.screen.contains(label), "{label}:\n{}", ran.screen);
        assert_ne!(active_generation(&tree), active.dir, "{label}");
        assert!(!active.dir.exists(), "{label}");
        let calls = uv_calls(&tree);
        let install: Vec<&String> = calls
            .iter()
            .filter(|call| call.starts_with("python install"))
            .collect();
        let sync = calls
            .iter()
            .find(|call| call.starts_with("pip sync"))
            .expect("sync");
        assert!(sync.contains(" --no-cache "), "{label}: {sync}");
        // uv's cache is cleared first, in its own step: the venv's files
        // are hardlinks into it, so damage to one would come back in the
        // next normal build.
        let clean = calls
            .iter()
            .position(|call| call.starts_with("cache clean"))
            .unwrap_or_else(|| panic!("{label}: {calls:?}"));
        let first_build = calls
            .iter()
            .position(|call| call.starts_with("python install") || call.starts_with("venv "))
            .expect("build");
        assert!(clean < first_build, "{label}: {calls:?}");
        assert!(
            ran.screen.contains("uv cache cleared"),
            "{label}:\n{}",
            ran.screen
        );
        let python = entries(&tree.layout.python_releases);
        if force == Force::Venv {
            assert!(install.is_empty(), "{install:?}");
            assert_eq!(python, [sb_python_env::testing::PYTHON_RELEASE]);
        } else {
            assert_eq!(install.len(), 1, "{calls:?}");
            assert!(
                install[0].contains(" --reinstall --no-cache "),
                "{install:?}"
            );
            assert_eq!(python.len(), 1, "{python:?}");
            assert_ne!(python, [sb_python_env::testing::PYTHON_RELEASE]);
        }
    }
}

/// A real directory at the active path is the legacy layout: unhealthy,
/// adopted at activation and then cleaned away with the other old
/// generations, as sb-go does.
#[tokio::test]
async fn a_legacy_directory_is_adopted_and_replaced() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.venv(&tree.layout.active_venv);
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, None, Force::None).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(ran.screen.contains("update required"), "{}", ran.screen);
    let meta = std::fs::symlink_metadata(&tree.layout.active_venv).expect("active");
    assert!(meta.file_type().is_symlink());
    let generation = active_generation(&tree);
    let id = generation.file_name().expect("id").to_string_lossy();
    assert_eq!(entries(&tree.layout.ansible_releases), [id.as_ref()]);
}

/// The previous manifest's commands the new one lacks lose their
/// wrappers, only those with sb's header: a user's own script stays.
#[tokio::test]
async fn stale_wrappers_go() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let mut previous = active.manifest.clone();
    previous.exported_commands.push("ansible-lint".to_string());
    tree.manifest(&active.dir, &previous);
    let managed: Vec<String> = COMMANDS.iter().map(ToString::to_string).collect();
    sb_python_env::wrappers::write_all(&tree.layout, &managed).expect("wrappers");
    let user_script = tree.layout.wrappers.join("ansible-lint");
    std::fs::write(&user_script, "#!/bin/sh\necho mine\n").expect("user script");
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, None, Force::Venv).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    // The stand-in sync exports ansible, apprise and certbot.
    assert!(!tree.layout.wrappers.join("ansible-playbook").exists());
    for kept in ["ansible", "apprise", "certbot"] {
        assert!(tree.layout.wrappers.join(kept).exists(), "{kept}");
    }
    assert_eq!(
        std::fs::read_to_string(&user_script).expect("kept"),
        "#!/bin/sh\necho mine\n"
    );
}

/// The state a failed or cancelled rebuild must leave as it found:
/// the active link, the wrappers' bytes and both `releases/`.
#[derive(Debug, PartialEq)]
struct Kept {
    link: std::path::PathBuf,
    wrappers: Vec<(String, Vec<u8>)>,
    generations: Vec<String>,
    pythons: Vec<String>,
}

fn kept(tree: &Tree) -> Kept {
    Kept {
        link: std::fs::read_link(&tree.layout.active_venv).expect("link"),
        wrappers: COMMANDS
            .iter()
            .map(|name| {
                let bytes = std::fs::read(tree.layout.wrappers.join(name)).expect("wrapper");
                ((*name).to_string(), bytes)
            })
            .collect(),
        generations: entries(&tree.layout.ansible_releases),
        pythons: entries(&tree.layout.python_releases),
    }
}

/// A healthy tree with its wrappers written, so a rebuild can be seen to
/// keep them.
fn wrapped_tree() -> Tree {
    let tree = Tree::new();
    let _ = tree.healthy();
    let managed: Vec<String> = COMMANDS.iter().map(ToString::to_string).collect();
    sb_python_env::wrappers::write_all(&tree.layout, &managed).expect("wrappers");
    tree
}

/// A rebuild failing at each build step leaves the previous generation
/// active, the wrappers as they were, and nothing of the new build (spec,
/// success criterion 5).
#[tokio::test]
async fn a_failed_rebuild_keeps_the_previous_generation() {
    let _spawns = SPAWNS.lock().await;
    for op in [
        "cache-clean",
        "python-install",
        "venv---python",
        "pip-sync",
        "pip-check",
    ] {
        let tree = wrapped_tree();
        let before = kept(&tree);
        tree.fail(op);
        let env = tree.env();
        let ran = in_step(async |step| {
            reconcile_venv(step, &env, None, Force::Python).await?;
            Ok(())
        })
        .await;
        assert!(ran.error.is_some(), "{op}:\n{}", ran.screen);
        assert_eq!(kept(&tree), before, "{op}");
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

/// Ctrl-C during the sync: the build goes, the previous generation stays
/// active with its wrappers.
#[tokio::test]
async fn a_cancelled_rebuild_keeps_the_previous_generation() {
    let _spawns = SPAWNS.lock().await;
    let tree = wrapped_tree();
    let before = kept(&tree);
    tree.hang("pip-sync");
    let env = tree.env();
    let started = tree.root.path().join("hang/started");
    let ran = in_step_with(async |step, shutdown| {
        let cancel = async {
            appears(&started).await;
            shutdown.signalled(2);
        };
        let (result, ()) = tokio::join!(reconcile_venv(step, &env, None, Force::Venv), cancel);
        result?;
        Ok(())
    })
    .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert_eq!(kept(&tree), before);
}

/// A rebuild gives the tree to the Saltbox user, as the healthy path does.
#[tokio::test]
async fn a_rebuilt_tree_is_given_to_the_saltbox_user() {
    if !root() {
        return;
    }
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let _ = tree.healthy();
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, Some("nobody"), Force::Venv).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let venv = std::fs::read_link(&tree.layout.active_venv).expect("link");
    let uid = std::fs::symlink_metadata(venv.join("bin/python3"))
        .expect("stat")
        .uid();
    // nobody's uid on Ubuntu (base-passwd).
    assert_eq!(uid, 65534);
}

/// After a rebuild, the old generation's removal and the ownership pass
/// are sub-steps of their own, after the build's (spec, steps 15 and 16):
/// the run shows what it is doing until its end. Ownership runs for a
/// user given, and is skipped for an unknown one, as here.
#[tokio::test]
async fn a_rebuild_shows_the_removal_and_the_ownership_as_steps() {
    let _spawns = SPAWNS.lock().await;
    for force in [Force::Venv, Force::Python] {
        let tree = Tree::new();
        let _ = tree.healthy();
        let env = tree.env();
        let ran = in_step(async |step| {
            reconcile_venv(step, &env, Some(UNKNOWN), force).await?;
            Ok(())
        })
        .await;
        assert_eq!(ran.error, None, "{force:?}:\n{}", ran.screen);
        let at = |line: &str| {
            ran.screen
                .find(line)
                .unwrap_or_else(|| panic!("{force:?}: {line}:\n{}", ran.screen))
        };
        let synced = at("Syncing hashed Saltbox requirements");
        let removed = at("Old environments cleaned up");
        let owned = at("Ownership set");
        assert!(synced < removed && removed < owned, "{}", ran.screen);
    }
}

/// A failed cleanup fails its own step with sb-go's words, and no
/// ownership step follows.
#[tokio::test]
async fn a_failed_cleanup_fails_its_step() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    // Not the obsolete link: the cleanup refuses it.
    std::fs::create_dir(tree.layout.ansible.join("previous")).expect("previous");
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, Some(UNKNOWN), Force::Venv).await?;
        Ok(())
    })
    .await;
    assert!(ran.error.is_some(), "{}", ran.screen);
    assert!(
        ran.screen.contains("✗ Cleaning up old environments"),
        "{}",
        ran.screen
    );
    assert!(
        ran.screen.contains("clean old Ansible generations: "),
        "{}",
        ran.screen
    );
    assert!(!ran.screen.contains("ownership"), "{}", ran.screen);
    // The new generation is active all the same: the run failed, the
    // venv works.
    assert_ne!(active_generation(&tree), active.dir);
}

/// Without a user (gha) there is no ownership step.
#[tokio::test]
async fn without_a_user_a_rebuild_has_no_ownership_step() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let _ = tree.healthy();
    let env = tree.env();
    let ran = in_step(async |step| {
        reconcile_venv(step, &env, None, Force::Venv).await?;
        Ok(())
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert!(
        ran.screen.contains("Old environments cleaned up"),
        "{}",
        ran.screen
    );
    for label in ["Setting ownership", "Ownership set"] {
        assert!(!ran.screen.contains(label), "{label}:\n{}", ran.screen);
    }
}

/// A user no host has: the ownership pass runs and skips.
const UNKNOWN: &str = "sb-rust-user-that-does-not-exist-7fcd24d1";

/// The active venv as found: a dangling link and an incomplete manifest
/// are unhealthy, as sb-go has them; so are a file at the active path and
/// a generation whose manifest is missing or isn't JSON, which sb-go
/// fails on (spec, difference 17): each is rebuilt, by the normal path
/// and by a forced one, and the old generation is cleaned away.
#[tokio::test]
async fn the_active_venv_is_inspected_as_sb_go_does() {
    type Breaks = fn(&Path);
    let _spawns = SPAWNS.lock().await;
    let run = async |tree: &Tree, force: Force| {
        let env = tree.env();
        in_step(async |step| {
            reconcile_venv(step, &env, None, force).await?;
            Ok(())
        })
        .await
    };
    let tree = Tree::new();
    tree.activate(&tree.root.path().join("gone/venv"));
    let ran = run(&tree, Force::None).await;
    assert!(ran.screen.contains("update required"), "{}", ran.screen);

    let tree = Tree::new();
    let active = tree.healthy();
    let mut value: serde_json::Value = serde_json::to_value(&active.manifest).expect("json");
    value
        .as_object_mut()
        .expect("object")
        .remove("saltbox_commit");
    std::fs::write(active.dir.join("manifest.json"), value.to_string()).expect("manifest");
    let ran = run(&tree, Force::None).await;
    assert!(ran.screen.contains("update required"), "{}", ran.screen);

    for force in [Force::None, Force::Venv, Force::Python] {
        let tree = Tree::new();
        std::fs::create_dir_all(&tree.layout.ansible).expect("ansible");
        std::fs::write(&tree.layout.active_venv, "a file").expect("file");
        let ran = run(&tree, force).await;
        assert_eq!(ran.error, None, "a file, {force:?}:\n{}", ran.screen);
        let generation = active_generation(&tree);
        assert!(generation.starts_with(&tree.layout.ansible_releases));
    }

    let breaks: [(&str, Breaks); 3] = [
        ("missing", |manifest| {
            std::fs::remove_file(manifest).expect("rm");
        }),
        ("not JSON", |manifest| {
            std::fs::write(manifest, "{ not json").expect("garbage");
        }),
        ("empty", |manifest| {
            std::fs::write(manifest, "").expect("empty");
        }),
    ];
    for (what, breaks) in breaks {
        for force in [Force::None, Force::Venv, Force::Python] {
            let tree = Tree::new();
            let active = tree.healthy();
            breaks(&active.dir.join("manifest.json"));
            let ran = run(&tree, force).await;
            assert_eq!(ran.error, None, "{what}, {force:?}:\n{}", ran.screen);
            if force == Force::None {
                assert!(ran.screen.contains("update required"), "{}", ran.screen);
            }
            assert_ne!(active_generation(&tree), active.dir, "{what}, {force:?}");
            assert!(!active.dir.exists(), "{what}, {force:?}: cleaned away");
        }
    }

    // A manifest that can't be read for another reason still fails the
    // inspection with sb-go's words.
    let tree = Tree::new();
    let active = tree.healthy();
    let manifest = active.dir.join("manifest.json");
    std::fs::remove_file(&manifest).expect("rm");
    std::fs::create_dir(&manifest).expect("a directory");
    let ran = run(&tree, Force::None).await;
    let error = ran.error.expect("unreadable");
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

/// A wrong uv whose install fails (here GitHub knows no such release)
/// fails its step (module build M18 replaced M15's "a wrong uv fails").
#[tokio::test]
async fn a_failed_uv_install_fails_its_step() {
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
            .contains("error fetching uv 0.13.4 release metadata: "),
        "{}",
        ran.screen
    );
    assert!(ran.error.is_some());
}

/// Everything else under both `releases/` goes, a stray file or link as
/// well as old directories; a link is removed, never followed.
#[tokio::test]
async fn cleanup_removes_stray_entries_too() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    let active = tree.healthy();
    let outside = tree.root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside");
    std::fs::write(outside.join("kept"), "").expect("kept");
    std::fs::write(tree.layout.ansible_releases.join("stray"), "").expect("stray");
    std::os::unix::fs::symlink(&outside, tree.layout.python_releases.join("link")).expect("link");
    activate::cleanup_generations(&tree.layout).expect("cleaned");
    assert!(!tree.layout.ansible_releases.join("stray").exists());
    assert!(std::fs::symlink_metadata(tree.layout.python_releases.join("link")).is_err());
    assert!(outside.join("kept").exists());
    assert!(active.dir.exists());
    assert!(active.python_install.exists());
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
