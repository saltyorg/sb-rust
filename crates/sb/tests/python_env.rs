//! `sb update-venv` through the built binary (module build M14): its help
//! and hidden state as any run, and a healthy stand-in tree kept, end to
//! end, as root (`SB_TEST_ROOT=1`). The tree is sb-python-env's testing
//! one, under `SB_TEST_PYTHON_ENV_ROOT` (read only by the sb `cargo test`
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
