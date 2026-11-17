//! `sb branch-sandbox <name>` through the built binary (module build M5;
//! sb-ops' branch command since M79):
//! its help and arguments as any run, and the switch end to end on a
//! local Sandbox checkout (a `file://` remote in a temp dir) found through
//! `SB_SALTBOX_DIR`'s inventory, owned by accounts.yml's user afterwards
//! (root runs: `SB_TEST_ROOT=1`, logging under the test's directory). No
//! test reaches a network remote.

mod common;
mod root_gate;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use sb_python_env::testing::{ROOT_VAR, Tree};
use sb_release::testing::BASES_VARIABLE;

/// What a switch runs besides git (ops spec, the branch commands' steps 4
/// and 5): python-env's stand-in tree with a healthy venv for Saltbox's
/// runtime, its fake GitHub's bases for the release client a test build
/// needs, and a stand-in ansible-playbook for the tags; with Saltbox's
/// safe.directory entry for root's git (repo spec, assumption 2) in the
/// environment's config, as the checkout is the Saltbox user's after the
/// switch.
struct Tools {
    tree: Tree,
    ansible: PathBuf,
}

impl Tools {
    fn new(dir: &Path) -> Self {
        let tree = Tree::new();
        let _ = tree.healthy();
        let ansible = sb_ops::testing::ansible_playbook(&dir.join("stand-ins"));
        Self { tree, ansible }
    }

    /// The built sb with the tools.
    fn sb(&self) -> Command {
        let mut cmd = common::sb();
        cmd.env(ROOT_VAR, self.tree.root.path())
            .env(BASES_VARIABLE, self.tree.bases())
            .env("SB_ANSIBLE_PLAYBOOK", &self.ansible)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "safe.directory")
            .env("GIT_CONFIG_VALUE_0", "*");
        cmd
    }
}

/// `git <args>` in `dir`, which must succeed; its stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        // None of the host's git settings: its global and system config
        // files (commit signing, credential helpers, rewrites) and its
        // per-user ignore and attributes files stay out, so a commit here
        // never asks for a key (the gate's mutation sandbox has no agent
        // to sign with) and `git add` takes every fixture file.
        // GIT_CONFIG_GLOBAL and GIT_CONFIG_SYSTEM set to /dev/null skip
        // those config files (git 2.32+):
        // https://git-scm.com/docs/git#Documentation/git.txt-GITCONFIGGLOBAL
        // The ignore and attributes files are read without any config,
        // from $XDG_CONFIG_HOME/git, unless these name others:
        // https://git-scm.com/docs/git-config#Documentation/git-config.txt-coreexcludesFile
        .args(["-c", "core.excludesFile=/dev/null"])
        .args(["-c", "core.attributesFile=/dev/null"])
        .args(["-c", "user.name=sb", "-c", "user.email=sb@example.invalid"])
        .args(["-c", "init.defaultBranch=master"])
        // The checkout belongs to nobody after the switch; Saltbox gives
        // root's git a safe.directory entry for it (repo spec,
        // assumption 2), the test this one-off option.
        .args(["-c", "safe.directory=*"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A Saltbox root whose inventory puts the appdata path in the temp dir,
/// accounts.yml naming `user`, and a single-branch Sandbox checkout of a
/// remote with `master` and `develop`; develop's commit.
fn saltbox(dir: &Path, user: &str) -> (PathBuf, PathBuf, String) {
    let root = dir.join("saltbox");
    let appdata = dir.join("appdata");
    let remote = dir.join("remote.git");
    let work = dir.join("work");
    std::fs::create_dir_all(root.join("inventories/host_vars")).expect("inventory dir");
    std::fs::write(
        root.join("inventories/host_vars/localhost.yml"),
        format!("server_appdata_path: {}\n", appdata.display()),
    )
    .expect("inventory");
    std::fs::write(
        root.join("accounts.yml"),
        format!("user:\n  name: {user}\n"),
    )
    .expect("accounts");
    std::fs::create_dir(&appdata).expect("appdata");
    git(
        dir,
        &["init", "--quiet", "--bare", remote.to_str().expect("utf-8")],
    );
    git(dir, &["init", "--quiet", work.to_str().expect("utf-8")]);
    std::fs::write(work.join("sandbox.yml"), "master\n").expect("write");
    git(&work, &["add", "."]);
    git(&work, &["commit", "--quiet", "-m", "first"]);
    let url = format!("file://{}", remote.display());
    git(&work, &["remote", "add", "origin", &url]);
    git(&work, &["push", "--quiet", "origin", "master"]);
    git(&work, &["checkout", "--quiet", "-b", "develop"]);
    std::fs::write(work.join("sandbox.yml"), "develop\n").expect("write");
    git(&work, &["commit", "--quiet", "-am", "develop"]);
    git(&work, &["push", "--quiet", "origin", "develop"]);
    let develop = git(&work, &["rev-parse", "develop"]);
    let checkout = appdata.join("sandbox");
    git(
        dir,
        &[
            "clone",
            "--quiet",
            "--depth",
            "1",
            "-b",
            "master",
            &url,
            checkout.to_str().expect("utf-8"),
        ],
    );
    (root, checkout, develop)
}

#[test]
fn help_is_sb_gos_and_the_name_is_required() {
    common::sb()
        .args(["branch-sandbox", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Change the branch used by Sandbox",
        ));
    common::sb().arg("branch-sandbox").assert().code(2);
    common::sb()
        .args(["branch-sandbox", "a", "b"])
        .assert()
        .code(2);
}

#[test]
fn the_sandbox_is_switched_and_owned_by_the_saltbox_user() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = Tools::new(dir.path());
    let (root, checkout, develop) = saltbox(dir.path(), "nobody");
    tools
        .sb()
        .args(["branch-sandbox", "develop", "--color=never"])
        .env("SB_SALTBOX_DIR", &root)
        .assert()
        .success()
        .stderr(
            predicate::str::contains("Sandbox repository switched to develop")
                .and(predicate::str::contains(
                    "Sandbox repository updated (develop)",
                ))
                .and(predicate::str::contains("Updating Sandbox tags cache"))
                .and(predicate::str::contains("Ansible virtual environment").not()),
        );
    let cache = sb_ansible::TagCache::load(&root);
    let entry = cache
        .entry(&sb_ansible::Repo::sandbox(
            checkout.parent().expect("appdata"),
        ))
        .expect("the tags cached");
    assert_eq!(entry.commit, develop);
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), develop);
    assert_eq!(
        git(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "develop"
    );
    // nobody's uid on Ubuntu (base-passwd).
    let owner = |path: &Path| std::fs::symlink_metadata(path).expect("stat").uid();
    for path in [
        checkout.clone(),
        checkout.join("sandbox.yml"),
        checkout.join(".git/HEAD"),
    ] {
        assert_eq!(owner(&path), 65534, "{}", path.display());
    }
}

#[test]
fn a_refused_name_fails_before_anything_changes() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = Tools::new(dir.path());
    let (root, checkout, _) = saltbox(dir.path(), "nobody");
    let before = git(&checkout, &["rev-parse", "HEAD"]);
    tools
        .sb()
        .args(["branch-sandbox", "--color=never", "--", "-x"])
        .env("SB_SALTBOX_DIR", &root)
        .assert()
        .code(1)
        .stderr(predicate::str::contains("'-x' is not a valid branch name"));
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), before);
    assert_ne!(
        std::fs::symlink_metadata(&checkout).expect("stat").uid(),
        65534
    );
}

#[test]
fn a_missing_user_fails_before_anything_changes() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = Tools::new(dir.path());
    let (root, checkout, _) = saltbox(dir.path(), "nobody");
    let before = git(&checkout, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("accounts.yml"), "other: 1\n").expect("accounts");
    tools
        .sb()
        .args(["branch-sandbox", "develop", "--color=never"])
        .env("SB_SALTBOX_DIR", &root)
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "user section not found in accounts.yml",
        ));
    assert_eq!(git(&checkout, &["rev-parse", "HEAD"]), before);
    assert_ne!(
        std::fs::symlink_metadata(&checkout).expect("stat").uid(),
        65534
    );
}

#[test]
fn a_missing_checkout_is_sb_gos_error() {
    if !root_gate::requested() {
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = Tools::new(dir.path());
    let (root, checkout, _) = saltbox(dir.path(), "nobody");
    std::fs::remove_dir_all(&checkout).expect("remove");
    tools
        .sb()
        .args(["branch-sandbox", "develop", "--color=never"])
        .env("SB_SALTBOX_DIR", &root)
        .assert()
        .code(1)
        .stderr(predicate::str::contains(format!(
            "inspect required directory {}",
            checkout.display()
        )));
}
