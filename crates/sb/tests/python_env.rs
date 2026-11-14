//! python-env's commands through the built binary (module build M14 and
//! M16): their help and hidden state as any run; `sb update-venv` keeping
//! a healthy stand-in tree, and `sb reinstall-venv` and `sb
//! reinstall-python` rebuilding it, end to end, as root
//! (`SB_TEST_ROOT=1`). The tree is sb-python-env's testing one, under `SB_TEST_PYTHON_ENV_ROOT` (read only by the sb `cargo test`
//! builds, with python-env's `testing` feature): every program is a
//! stand-in, nothing touches `/srv`, `/usr/local/bin`, apt or the network.

mod common;
mod root_gate;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use sb_python_env::testing::{ROOT_VAR, Tree};

#[test]
fn update_venv_is_hidden_and_takes_no_arguments() {
    common::sb()
        .args(["--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains("update-venv").not());
    common::sb()
        .args(["update-venv", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Update the Ansible virtual environment from the current Saltbox checkout and refresh saltbox.fact without updating Git repositories",
        ));
    common::sb().args(["update-venv", "extra"]).assert().code(2);
    common::sb().args(["update-venv", "-v"]).assert().code(2);
}

/// The tree with accounts.yml naming `user` in its checkout.
fn tree(user: &str) -> Tree {
    let tree = Tree::new();
    std::fs::write(
        tree.layout.checkout.join("accounts.yml"),
        format!("user:\n  name: {user}\n"),
    )
    .expect("accounts");
    tree
}

#[test]
fn a_healthy_tree_is_up_to_date() {
    if !root_gate::requested() {
        return;
    }
    let tree = tree("nobody");
    let active = tree.healthy();
    common::sb()
        .args(["update-venv", "--color=never"])
        .env(ROOT_VAR, tree.root.path())
        .env("SB_SALTBOX_DIR", &tree.layout.checkout)
        .assert()
        .success()
        .stderr(
            predicate::str::contains("Ansible virtual environment was already up to date").and(
                predicate::str::contains("Ansible virtual environment and saltbox.fact are ready"),
            ),
        );
    assert_eq!(
        std::fs::read_link(&tree.layout.active_venv).expect("link"),
        active.venv
    );
    assert!(tree.layout.wrappers.join("ansible-playbook").exists());
}

#[test]
fn a_missing_user_is_sb_gos_error() {
    if !root_gate::requested() {
        return;
    }
    let tree = tree("nobody");
    let _ = tree.healthy();
    std::fs::write(tree.layout.checkout.join("accounts.yml"), "other: 1\n").expect("accounts");
    common::sb()
        .args(["update-venv", "--color=never"])
        .env(ROOT_VAR, tree.root.path())
        .env("SB_SALTBOX_DIR", &tree.layout.checkout)
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "error getting saltbox user: user section not found in accounts.yml",
        ));
    // Nothing ran: no wrapper written.
    assert!(!tree.layout.wrappers.join("ansible-playbook").exists());
}

/// Both reinstall commands are listed, with sb-go's help, and take no
/// arguments and no `-v` (python-env decision 9).
#[test]
fn the_reinstall_commands_take_no_arguments() {
    for (name, about) in [
        (
            "reinstall-venv",
            "Reinstall the Ansible virtual environment",
        ),
        (
            "reinstall-python",
            "Reinstall the Python version used by Saltbox and related Ansible virtual environment using uv",
        ),
    ] {
        common::sb()
            .args(["--help", "--color=never"])
            .assert()
            .success()
            .stdout(predicate::str::contains(name));
        common::sb()
            .args([name, "--help", "--color=never"])
            .assert()
            .success()
            .stdout(predicate::str::contains(about));
        common::sb().args([name, "extra"]).assert().code(2);
        common::sb().args([name, "-v"]).assert().code(2);
    }
}

/// `sb <command>` on the tree, as root.
fn run(tree: &Tree, command: &str) -> assert_cmd::assert::Assert {
    common::sb()
        .args([command, "--color=never"])
        .env(ROOT_VAR, tree.root.path())
        .env("SB_SALTBOX_DIR", &tree.layout.checkout)
        .assert()
}

/// The generation directory the active link names.
fn active_generation(tree: &Tree) -> std::path::PathBuf {
    let venv = std::fs::read_link(&tree.layout.active_venv).expect("link");
    venv.parent().expect("generation").to_path_buf()
}

/// `sb reinstall-venv` rebuilds a healthy venv on its Python, with
/// sb-go's labels.
#[test]
fn reinstall_venv_rebuilds_the_venv() {
    if !root_gate::requested() {
        return;
    }
    let tree = tree("nobody");
    let active = tree.healthy();
    run(&tree, "reinstall-venv").success().stderr(
        predicate::str::contains("Ansible virtual environment recreation requested").and(
            predicate::str::contains("Ansible virtual environment reinstalled"),
        ),
    );
    assert_ne!(active_generation(&tree), active.dir);
    assert!(!active.dir.exists());
    assert!(active.python_install.exists(), "its Python is reused");
}

/// `sb reinstall-python` rebuilds the Python and the venv, in sb-go's two
/// steps.
#[test]
fn reinstall_python_rebuilds_python_and_the_venv() {
    if !root_gate::requested() {
        return;
    }
    let tree = tree("nobody");
    let active = tree.healthy();
    run(&tree, "reinstall-python").success().stderr(
        predicate::str::contains("Python and Ansible virtual environment recreation requested")
            .and(predicate::str::contains("Installing Python 3.12.7"))
            .and(predicate::str::contains(
                "Ansible virtual environment recreated",
            ))
            .and(predicate::str::contains(
                "Saltbox Python and Ansible environment reinstalled",
            )),
    );
    assert_ne!(active_generation(&tree), active.dir);
    assert!(!active.python_install.exists(), "a new Python replaces it");
}

/// A failed rebuild exits 1 with sb-go's wrapping under the step that
/// ran it, the previous generation still active. The staged check fails
/// here (the stand-in `uv pip check`): a failing nested step (the sync)
/// shows its own cause, and the steps above it end quietly.
#[test]
fn a_failed_reinstall_keeps_the_previous_venv() {
    if !root_gate::requested() {
        return;
    }
    for (command, failure) in [
        (
            "reinstall-venv",
            "✗ Reinstalling Ansible virtual environment",
        ),
        ("reinstall-python", "✗ Ansible virtual environment"),
    ] {
        let tree = tree("nobody");
        let active = tree.healthy();
        std::fs::create_dir_all(tree.root.path().join("fail")).expect("fail dir");
        std::fs::write(tree.root.path().join("fail/pip-check"), "").expect("marker");
        run(&tree, command).code(1).stderr(
            // The error block: the chain, one cause a line.
            predicate::str::contains("  error managing Ansible venv\n")
                .and(predicate::str::contains(
                    "  validate staged Ansible environment: \
                     installed Python packages are incompatible: ",
                ))
                .and(predicate::str::contains(failure)),
        );
        assert_eq!(active_generation(&tree), active.dir, "{command}");
    }
}

/// Both read the Saltbox user first: a broken accounts.yml stops them
/// before anything runs.
#[test]
fn a_reinstall_without_a_user_is_sb_gos_error() {
    if !root_gate::requested() {
        return;
    }
    for command in ["reinstall-venv", "reinstall-python"] {
        let tree = tree("nobody");
        let active = tree.healthy();
        std::fs::write(tree.layout.checkout.join("accounts.yml"), "other: 1\n").expect("accounts");
        run(&tree, command).code(1).stderr(predicate::str::contains(
            "error getting saltbox user: user section not found in accounts.yml",
        ));
        assert!(
            tree.calls_made().is_empty(),
            "{command}: {:?}",
            tree.calls_made()
        );
        assert_eq!(active_generation(&tree), active.dir);
    }
}
