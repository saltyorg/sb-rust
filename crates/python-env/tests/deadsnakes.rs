//! The 22.04 deadsnakes cleanup (python-env spec, "Deadsnakes cleanup";
//! module build M20) on the stand-in tree: dpkg-query's rows and
//! apt-cache's policies from the fixtures, apt-get recording its calls,
//! and the tree's own `etc/apt` and `var/backups`. Nothing here touches the
//! host's apt or `/etc/apt`.

use std::path::{Path, PathBuf};

use sb_cli_core::cancel::Shutdown;
use sb_python_env::testing::Tree;
use sb_python_env::{Env, cleanup_deadsnakes, reconcile_runtime};
use sb_term_ui::{Labels, Options, Runner, Step, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The run id the tests' backups are named by.
const RUN: &str = "0a1b2c3d";

/// The PPA's line in a source file.
const PPA: &str = "deb https://ppa.launchpadcontent.net/deadsnakes/ppa/ubuntu jammy main\n";

/// The version every fixture row and policy names.
const VERSION: &str = "3.12.12-1+jammy1";

/// What a run left: its body's error chain, what it returned and the
/// screen.
struct Ran {
    error: Option<String>,
    cleaned: Option<bool>,
    screen: String,
}

/// `body` in a step of a runner drawing to memory.
async fn in_step(body: impl AsyncFnOnce(&mut Step) -> anyhow::Result<Option<bool>>) -> Ran {
    let term = indicatif::InMemoryTerm::new(80, 400);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb test", &shutdown, None, options);
    let (mut error, mut cleaned) = (None, None);
    let labels = Labels::new(
        "Checking for old deadsnakes Python packages",
        "Checking for old deadsnakes Python packages",
    );
    let _ = runner
        .step(labels, async |step| match body(step).await {
            Ok(done) => {
                cleaned = done;
                Ok(())
            }
            Err(err) => {
                error = Some(format!("{err:#}"));
                Err(err)
            }
        })
        .await;
    runner.finish();
    Ran {
        error,
        cleaned,
        screen: term.contents(),
    }
}

/// `cleanup_deadsnakes` on `env`.
async fn cleanup(env: &Env<'_>) -> Ran {
    in_step(async |step| Ok(Some(cleanup_deadsnakes(step, env).await?))).await
}

/// A fixture's text.
fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deadsnakes")
        .join(name);
    std::fs::read_to_string(path).expect("fixture")
}

/// A 22.04 tree with the fixture's packages: dpkg's rows, and each
/// candidate's policy (the PPA's, but the old hostname for
/// `libpython3.12t64:amd64`, no source left with the `+jammy1` suffix for
/// `2to3.12`, and another origin for `python3.12-distutils`, kept). After
/// a removal, dpkg holds the rows that aren't deadsnakes'.
fn jammy_tree() -> Tree {
    let tree = Tree::new();
    tree.release("22.04");
    let rows = fixture("dpkg-query.txt");
    tree.rows(&rows);
    let kept: Vec<&str> = rows
        .lines()
        .filter(|line| {
            !REMOVED
                .iter()
                .any(|name| line.starts_with(&format!("{name}\t")))
        })
        .collect();
    tree.rows_after_remove(&(kept.join("\n") + "\n"));
    for (name, policy) in [
        ("2to3.12", "policy-vanished.txt"),
        ("idle-python3.12", "policy-ppa.txt"),
        ("libpython3.12-stdlib:amd64", "policy-ppa.txt"),
        ("libpython3.12t64:amd64", "policy-old-host.txt"),
        ("python3.12", "policy-ppa.txt"),
        ("python3.12-distutils", "policy-other-version.txt"),
    ] {
        tree.policy(name, &fixture(policy));
    }
    tree
}

/// The packages [`jammy_tree`] removes, in name order.
const REMOVED: [&str; 5] = [
    "2to3.12",
    "idle-python3.12",
    "libpython3.12-stdlib:amd64",
    "libpython3.12t64:amd64",
    "python3.12",
];

/// `tree`'s Env with the fixed run id.
fn env(tree: &Tree) -> Env<'_> {
    Env {
        run_id: RUN.to_owned(),
        ..tree.env()
    }
}

/// The apt-get calls made, without the program and sb's `-o` options.
fn apt_get(tree: &Tree) -> Vec<String> {
    tree.calls_made()
        .iter()
        .filter(|call| call.contains("/stand-ins/apt-get "))
        .map(|call| {
            let args: Vec<&str> = call.split(' ').skip(1).collect();
            let mut kept = Vec::new();
            let mut skip = false;
            for arg in args {
                if skip {
                    skip = false;
                } else if arg == "-o" {
                    skip = true;
                } else {
                    kept.push(arg);
                }
            }
            kept.join(" ")
        })
        .collect()
}

/// The deadsnakes backup directory of the run.
fn backup(tree: &Tree) -> PathBuf {
    tree.backups().join("sb/deadsnakes").join(RUN)
}

/// On 22.04: the deadsnakes packages removed, then autoremove; the source
/// files mentioning deadsnakes (a regular one, a symlinked one, one in
/// sb-go's upper case) moved aside, then `apt-get update`; another
/// source stays.
#[tokio::test]
async fn on_jammy_the_packages_go_and_the_sources_move_aside() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    let sources_d = tree.sources_d();
    std::fs::write(sources_d.join("deadsnakes-ubuntu-ppa-jammy.list"), PPA).expect("write");
    let upper = "URIs: https://ppa.launchpadcontent.net/DEADSNAKES/ppa/ubuntu\n";
    std::fs::write(sources_d.join("upper.sources"), upper).expect("write");
    let target = tree.root.path().join("linked.list");
    std::fs::write(&target, PPA).expect("write");
    std::os::unix::fs::symlink(&target, sources_d.join("linked.list")).expect("symlink");
    std::fs::write(sources_d.join("docker.list"), "docker\n").expect("write");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.cleaned, Some(true));
    assert_eq!(
        apt_get(&tree),
        [
            format!("remove -y {}", REMOVED.join(" ")),
            "autoremove -y".to_owned(),
            "update".to_owned(),
        ]
    );
    let line = format!(
        "Moved 3 deadsnakes repository files to {}",
        backup(&tree).display()
    );
    assert!(ran.screen.contains(&line), "{}", ran.screen);
    let mut moved: Vec<_> = std::fs::read_dir(backup(&tree))
        .expect("backup")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    moved.sort();
    assert_eq!(
        moved,
        [
            "deadsnakes-ubuntu-ppa-jammy.list",
            "linked.list",
            "upper.sources"
        ]
    );
    assert!(
        std::fs::symlink_metadata(backup(&tree).join("linked.list"))
            .expect("lstat")
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&target).expect("target"), PPA);
    let left: Vec<_> = std::fs::read_dir(&sources_d)
        .expect("sources")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(left, ["docker.list"]);
}

/// No deadsnakes package and no source mentioning it: nothing removed,
/// nothing updated, no backup directory, and `false`.
#[tokio::test]
async fn with_nothing_to_clean_nothing_runs() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.release("22.04");
    tree.rows("python3.13\tpython3.13\tii \t3.13.7-1\n");
    std::fs::write(tree.sources_d().join("docker.list"), "docker\n").expect("write");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.cleaned, Some(false));
    assert_eq!(apt_get(&tree), Vec::<String>::new());
    assert!(!tree.backups().join("sb").exists());
}

/// Packages only: removed, and `true`, with no update.
#[tokio::test]
async fn packages_alone_are_removed_without_an_update() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    let _ = tree.sources_d();
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.cleaned, Some(true));
    assert_eq!(apt_get(&tree).len(), 2, "{:?}", apt_get(&tree));
    assert!(!ran.screen.contains("Moved"), "{}", ran.screen);
}

/// A source file alone: moved, then the update, and `true`.
#[tokio::test]
async fn a_source_alone_is_moved_then_the_lists_updated() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.release("22.04");
    std::fs::write(tree.sources_d().join("ppa.list"), PPA).expect("write");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    assert_eq!(ran.cleaned, Some(true));
    assert_eq!(apt_get(&tree), ["update"]);
    assert!(backup(&tree).join("ppa.list").exists());
}

/// Any release but 22.04, and one that isn't supported: nothing is asked
/// and nothing moves, whatever is there.
#[tokio::test]
async fn on_any_other_release_nothing_is_done() {
    let _spawns = SPAWNS.lock().await;
    for release in ["24.04", "26.04", "20.04", "not a release"] {
        let tree = jammy_tree();
        tree.release(release);
        std::fs::write(tree.sources_d().join("ppa.list"), PPA).expect("write");
        let env = env(&tree);
        let ran = cleanup(&env).await;
        assert_eq!(ran.error, None, "{release}: {}", ran.screen);
        assert_eq!(ran.cleaned, Some(false), "{release}");
        assert_eq!(tree.calls_made(), Vec::<String>::new(), "{release}");
        assert!(tree.sources_d().join("ppa.list").exists(), "{release}");
    }
}

/// sb-go's package postcondition: a package still there after the
/// removal fails the cleanup, before any source moves.
#[tokio::test]
async fn a_package_left_after_removal_is_sb_gos_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    tree.rows_after_remove(&format!("python3.12\tpython3.12\tii \t{VERSION}\n"));
    std::fs::write(tree.sources_d().join("ppa.list"), PPA).expect("write");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(
        ran.error.as_deref(),
        Some("deadsnakes Python packages remain after cleanup: python3.12")
    );
    assert!(tree.sources_d().join("ppa.list").exists());
}

/// A failed removal: sb-go's text over apt's, and no autoremove.
#[tokio::test]
async fn a_failed_removal_stops_before_autoremove() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    tree.fail("apt-get-remove");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    let error = ran.error.expect("failed");
    assert!(
        error.starts_with("remove deadsnakes Python packages: "),
        "{error}"
    );
    assert_eq!(apt_get(&tree).len(), 1, "{:?}", apt_get(&tree));
}

/// A failed autoremove: sb-go's text.
#[tokio::test]
async fn a_failed_autoremove_is_sb_gos_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    tree.fail("apt-get-autoremove");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    let error = ran.error.expect("failed");
    assert!(
        error.starts_with("autoremove deadsnakes Python dependencies: "),
        "{error}"
    );
}

/// A package list that can't be read again after the removal: sb-go's
/// "verify" text over the cause.
#[tokio::test]
async fn a_failed_second_query_is_sb_gos_verify_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    tree.rows_after_remove("\tpython3.12\tii \t1\n");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(
        ran.error.as_deref(),
        Some(
            "verify deadsnakes package removal: parse installed packages: line 1: package name is empty"
        )
    );
}

/// A failed update after the move: sb-go's text, the files already in
/// the backup.
#[tokio::test]
async fn a_failed_update_after_the_move_is_sb_gos_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.release("22.04");
    std::fs::write(tree.sources_d().join("ppa.list"), PPA).expect("write");
    tree.fail("apt-get-update");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    let error = ran.error.expect("failed");
    assert!(
        error.starts_with(
            "remove deadsnakes repositories: update apt package lists after removing \
             deadsnakes repositories: failed to update package lists: "
        ),
        "{error}"
    );
    assert!(backup(&tree).join("ppa.list").exists());
}

/// sb-go's repository postcondition: a file mentioning deadsnakes back
/// after the update fails the cleanup, named.
#[tokio::test]
async fn a_source_back_after_the_update_is_sb_gos_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.release("22.04");
    let sources_d = tree.sources_d();
    std::fs::write(sources_d.join("ppa.list"), PPA).expect("write");
    let again = sources_d.join("again.list");
    tree.on_update(&format!(
        "printf 'deb http://ppa.launchpad.net/deadsnakes/ppa/ubuntu jammy main\\n' > '{}'",
        again.display()
    ));
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(
        ran.error,
        Some(format!(
            "remove deadsnakes repositories: deadsnakes repository files remain after cleanup: {}",
            again.display()
        ))
    );
}

/// A refused `sources.list.d`: sb-host's text under sb-go's, nothing
/// updated.
#[tokio::test]
async fn a_refused_sources_directory_is_named() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.release("22.04");
    let sources_d = tree.sources_d();
    std::fs::write(sources_d.join("ppa.list"), PPA).expect("write");
    std::fs::set_permissions(
        &sources_d,
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    )
    .expect("chmod");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(
        ran.error,
        Some(format!(
            "remove deadsnakes repositories: refusing to clean {}: writable by group or others",
            sources_d.display()
        ))
    );
    assert_eq!(apt_get(&tree), Vec::<String>::new());
}

/// A policy that doesn't list the installed version: sb-go's error.
#[tokio::test]
async fn an_undescribed_version_is_sb_gos_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    tree.policy("python3.12", "python3.12:\n  Installed: (none)\n");
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(
        ran.error,
        Some(format!(
            "apt policy for python3.12 does not describe installed version {VERSION}"
        ))
    );
    assert_eq!(apt_get(&tree), Vec::<String>::new());
}

/// Through `reconcile_runtime`, as `sb update-venv` runs it: the cleanup's
/// info line, then the venv and the fact as on any healthy tree.
#[tokio::test]
async fn the_runtime_step_says_what_the_cleanup_removed() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    let _ = tree.healthy();
    std::fs::write(tree.sources_d().join("ppa.list"), PPA).expect("write");
    let env = env(&tree);
    let ran = in_step(async |step| {
        reconcile_runtime(step, &env, None).await?;
        Ok(None)
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    for line in [
        "Removed old deadsnakes Python packages",
        "Moved 1 deadsnakes repository files to ",
        "Ansible virtual environment was already up to date",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
}

/// The cleanup's moved sources refresh the lists once, and nothing after
/// it in the runtime runs apt (libpq-dev dropped, 2026-11-21).
#[tokio::test]
async fn the_cleanups_refresh_is_the_runtimes_only_apt_update() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    let _ = tree.healthy();
    std::fs::write(tree.sources_d().join("ppa.list"), PPA).expect("write");
    let env = env(&tree);
    let ran = in_step(async |step| {
        reconcile_runtime(step, &env, None).await?;
        Ok(None)
    })
    .await;
    assert_eq!(ran.error, None, "{}", ran.screen);
    let calls = apt_get(&tree);
    let updates = calls.iter().filter(|call| *call == "update").count();
    assert_eq!(updates, 1, "{calls:?}");
    assert!(
        !calls.iter().any(|call| call.contains("install")),
        "{calls:?}"
    );
}

/// A cleanup failure stops the runtime's update under sb-go's text,
/// before the venv step.
#[tokio::test]
async fn a_failed_cleanup_is_sb_gos_runtime_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = jammy_tree();
    let _ = tree.healthy();
    tree.fail("apt-get-remove");
    let env = env(&tree);
    let ran = in_step(async |step| {
        reconcile_runtime(step, &env, None).await?;
        Ok(None)
    })
    .await;
    assert!(ran.error.is_some());
    for line in [
        "error cleaning up deadsnakes packages\n",
        "remove deadsnakes Python packages\n",
    ] {
        assert!(ran.screen.contains(line), "{line}:\n{}", ran.screen);
    }
    assert!(
        !ran.screen.contains("Preparing Ansible virtual environment"),
        "{}",
        ran.screen
    );
}

/// A source that names deadsnakes but isn't the PPA (a snapshot mirror,
/// a comment) stays, and so does the package it gives (sb-go's
/// `TestPolicyVersionRejectsNonCanonicalDeadsnakesName`): moving the file
/// would leave that package with no source on the next run, where its
/// `+jammy1` suffix would read as deadsnakes'.
#[tokio::test]
async fn a_source_that_is_not_the_ppa_stays_with_its_package() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.release("22.04");
    tree.rows(&format!("python3.12\tpython3.12\tii \t{VERSION}\n"));
    tree.policy("python3.12", &fixture("policy-snapshot.txt"));
    let sources_d = tree.sources_d();
    let snapshot = "deb https://packages.example.invalid/deadsnakes-snapshot jammy main\n";
    std::fs::write(sources_d.join("snapshot.list"), snapshot).expect("write");
    std::fs::write(sources_d.join("notes.list"), "# moved off deadsnakes\n").expect("write");
    let env = env(&tree);
    for _ in 0..2 {
        let ran = cleanup(&env).await;
        assert_eq!(ran.error, None, "{}", ran.screen);
        assert_eq!(ran.cleaned, Some(false));
    }
    assert_eq!(apt_get(&tree), Vec::<String>::new());
    assert!(sources_d.join("snapshot.list").exists());
    assert!(sources_d.join("notes.list").exists());
}

/// The re-check failing after the update (here `sources.list.d` made
/// writable by others meanwhile): sb-go's "verify" text over sb-host's.
#[tokio::test]
async fn a_failed_re_check_is_sb_gos_verify_error() {
    let _spawns = SPAWNS.lock().await;
    let tree = Tree::new();
    tree.release("22.04");
    let sources_d = tree.sources_d();
    std::fs::write(sources_d.join("ppa.list"), PPA).expect("write");
    tree.on_update(&format!("chmod 0777 '{}'", sources_d.display()));
    let env = env(&tree);
    let ran = cleanup(&env).await;
    assert_eq!(
        ran.error,
        Some(format!(
            "remove deadsnakes repositories: verify deadsnakes repository removal: \
             refusing to clean {}: writable by group or others",
            sources_d.display()
        ))
    );
}
