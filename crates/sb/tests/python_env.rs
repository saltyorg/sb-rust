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
use sb_python_env::testing::{ROOT_VAR, Tree, UV};
use sb_release::Arch;
use sb_release::testing::BASES_VARIABLE;

#[test]
fn update_venv_is_hidden_and_takes_no_arguments() {
    common::sb()
        .args(["--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains("update-venv").not());
    // The about wraps at 120 columns, as sb-go's (fang's) does.
    common::sb()
        .args(["update-venv", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  Update the Ansible virtual environment from the current Saltbox checkout and refresh saltbox.fact without updating Git\n  repositories\n",
        ));
    common::sb().args(["update-venv", "extra"]).assert().code(2);
    // sb-go's `-v`, accepted (the maintainer, 2026-11-18): `--help` after
    // it parses only if it does, and runs nothing.
    common::sb()
        .args(["update-venv", "-v", "--help"])
        .assert()
        .success();
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
        .env(BASES_VARIABLE, tree.bases())
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
        .env(BASES_VARIABLE, tree.bases())
        .env("SB_SALTBOX_DIR", &tree.layout.checkout)
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "error getting saltbox user: user section not found in accounts.yml",
        ));
    // Nothing ran: no wrapper written.
    assert!(!tree.layout.wrappers.join("ansible-playbook").exists());
}

/// On 22.04 `sb update-venv` runs the deadsnakes cleanup (module build
/// M20): the package removed, the PPA's source file moved into a backup
/// directory named by the run log's id (cli-core's 16 hex digits), both
/// lines shown.
#[test]
fn update_venv_cleans_up_deadsnakes_on_jammy() {
    if !root_gate::requested() {
        return;
    }
    let tree = tree("nobody");
    let _ = tree.healthy();
    tree.release("22.04");
    tree.rows("python3.12\tpython3.12\tii \t3.12.12-1+jammy1\n");
    tree.rows_after_remove("");
    tree.policy(
        "python3.12",
        " *** 3.12.12-1+jammy1 500\n        500 https://ppa.launchpadcontent.net/deadsnakes/ppa/ubuntu jammy/main amd64 Packages\n",
    );
    let ppa = "deb https://ppa.launchpadcontent.net/deadsnakes/ppa/ubuntu jammy main\n";
    std::fs::write(tree.sources_d().join("ppa.list"), ppa).expect("source");
    run(&tree, "update-venv").success().stderr(
        predicate::str::contains("Removed old deadsnakes Python packages")
            .and(predicate::str::contains(
                "Moved 1 deadsnakes repository files to ",
            ))
            .and(predicate::str::contains(
                "Ansible virtual environment and saltbox.fact are ready",
            )),
    );
    let runs: Vec<_> = std::fs::read_dir(tree.backups().join("sb/deadsnakes"))
        .expect("backups")
        .map(|entry| entry.expect("entry").path())
        .collect();
    let [run] = runs.as_slice() else {
        panic!("one run directory: {runs:?}");
    };
    let id = run
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    assert!(
        id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "{id}"
    );
    assert_eq!(
        std::fs::read_to_string(run.join("ppa.list")).expect("moved"),
        ppa
    );
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
        common::sb().args([name, "-v", "--help"]).assert().success();
    }
}

/// `sb <command>` on the tree, as root.
fn run(tree: &Tree, command: &str) -> assert_cmd::assert::Assert {
    common::sb()
        .args([command, "--color=never"])
        .env(ROOT_VAR, tree.root.path())
        .env(BASES_VARIABLE, tree.bases())
        .env("SB_SALTBOX_DIR", &tree.layout.checkout)
        .assert()
}

/// The generation directory the active link names.
fn active_generation(tree: &Tree) -> std::path::PathBuf {
    let venv = std::fs::read_link(&tree.layout.active_venv).expect("link");
    venv.parent().expect("generation").to_path_buf()
}

/// `sb reinstall-venv` rebuilds a healthy venv on its Python, with
/// sb-go's labels, and shows the cleanup and the ownership pass as steps.
#[test]
fn reinstall_venv_rebuilds_the_venv() {
    if !root_gate::requested() {
        return;
    }
    let tree = tree("nobody");
    let active = tree.healthy();
    run(&tree, "reinstall-venv").success().stderr(
        predicate::str::contains("Ansible virtual environment recreation requested")
            .and(predicate::str::contains("Old environments cleaned up"))
            .and(predicate::str::contains("Ownership set"))
            .and(predicate::str::contains(
                "Ansible virtual environment reinstalled",
            )),
    );
    assert_ne!(active_generation(&tree), active.dir);
    assert!(!active.dir.exists());
    assert!(active.python_install.exists(), "its Python is reused");
}

/// `sb reinstall-python` rebuilds the Python and the venv, in sb-go's two
/// steps, the cleanup and the ownership pass shown under them.
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
            .and(predicate::str::contains("Old environments cleaned up"))
            .and(predicate::str::contains("Ownership set"))
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
/// here (the stand-in `uv pip check`), in its own nested step, which
/// shows its own cause; the steps above it end quietly (difference 14).
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
        (
            "reinstall-python",
            "✗ Recreating Ansible virtual environment",
        ),
    ] {
        let tree = tree("nobody");
        let active = tree.healthy();
        std::fs::create_dir_all(tree.root.path().join("fail")).expect("fail dir");
        std::fs::write(tree.root.path().join("fail/pip-check"), "").expect("marker");
        run(&tree, command).code(1).stderr(
            // The failed step's block: its cause, the steps above quiet.
            predicate::str::contains("✗ Validating the new Ansible virtual environment")
                .and(predicate::str::contains("error managing Ansible venv").not())
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

/// A wrong uv is replaced by the pinned release, from the fake GitHub
/// (module build M18), and the run goes on with it.
#[test]
fn update_venv_installs_the_pinned_uv() {
    if !root_gate::requested() {
        return;
    }
    let tree = tree("nobody");
    let _ = tree.healthy();
    tree.uv("0.13.3", "0.13.3");
    tree.serve_uv(Arch::current().expect("arch"), &tree.uv_archive(UV, UV));
    run(&tree, "update-venv").success().stderr(
        predicate::str::contains("Ensuring uv 0.13.4 is installed").and(predicate::str::contains(
            "Ansible virtual environment was already up to date",
        )),
    );
    let uv = std::fs::read_to_string(&tree.layout.uv).expect("uv");
    assert!(uv.contains("echo 'uv 0.13.4 (stand-in)'"), "{uv}");
    let uvx = std::fs::read_to_string(&tree.layout.uvx).expect("uvx");
    assert!(uvx.contains("echo 'uvx 0.13.4 (stand-in)'"), "{uvx}");
}

/// `sb reinstall-facts` is listed with sb-go's help and takes no
/// arguments and no `-v` (python-env decision 9).
#[test]
fn reinstall_facts_takes_no_arguments() {
    common::sb()
        .args(["--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reinstall-facts"));
    common::sb()
        .args(["reinstall-facts", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Reinstall the Rust saltbox.fact file",
        ));
    common::sb()
        .args(["reinstall-facts", "extra"])
        .assert()
        .code(2);
    common::sb()
        .args(["reinstall-facts", "-v", "--help"])
        .assert()
        .success();
}

/// `sb reinstall-facts` replaces even an up-to-date fact, with
/// accounts.yml missing or invalid (spec, success criterion 8).
#[test]
fn reinstall_facts_works_without_a_user() {
    if !root_gate::requested() {
        return;
    }
    for accounts in [None, Some("user: [not, a, map\n")] {
        let tree = Tree::new();
        if let Some(text) = accounts {
            std::fs::create_dir_all(&tree.layout.checkout).expect("checkout");
            std::fs::write(tree.layout.checkout.join("accounts.yml"), text).expect("accounts");
        }
        let body = sb_python_env::testing::fact_binary("1.4.0");
        tree.serve_fact(Arch::current().expect("arch"), "v1.4.0", &body);
        tree.fact(&body);
        run(&tree, "reinstall-facts").success().stderr(
            predicate::str::contains("Reinstall forced.")
                .and(predicate::str::contains(
                    "saltbox.fact reinstalled successfully (version v1.4.0)",
                ))
                .and(predicate::str::contains("Reinstalling saltbox.fact")),
        );
        assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), body);
        let downloads = tree
            .fake
            .seen()
            .iter()
            .filter(|seen| seen.target.contains("/releases/download/"))
            .count();
        assert_eq!(downloads, 1, "{accounts:?}");
    }
}

/// A failed reinstall exits 1, the failing nested step naming the cause:
/// here the release has no fact for this architecture.
#[test]
fn a_failed_reinstall_facts_exits_1() {
    if !root_gate::requested() {
        return;
    }
    let tree = Tree::new();
    let other = match Arch::current().expect("arch") {
        Arch::Amd64 => Arch::Arm64,
        Arch::Arm64 => Arch::Amd64,
    };
    tree.serve_fact(other, "v1.4.0", b"x");
    run(&tree, "reinstall-facts")
        .code(1)
        .stderr(predicate::str::contains("is missing saltbox-facts-"));
}

/// `sb update-venv` repairs a fact edited at the released version, with
/// no metadata request beyond the one (success criterion 6).
#[test]
fn update_venv_repairs_a_tampered_fact() {
    if !root_gate::requested() {
        return;
    }
    let tree = tree("nobody");
    let _ = tree.healthy();
    let good = std::fs::read(&tree.layout.fact).expect("fact");
    let mut bad = good.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    tree.fact(&bad);
    run(&tree, "update-venv").success().stderr(
        predicate::str::contains("does not match its release").and(predicate::str::contains(
            "saltbox.fact reinstalled successfully",
        )),
    );
    assert_eq!(std::fs::read(&tree.layout.fact).expect("fact"), good);
    let api = tree
        .fake
        .seen()
        .iter()
        .filter(|seen| seen.target.starts_with("/api/"))
        .count();
    assert_eq!(api, 1);
}
