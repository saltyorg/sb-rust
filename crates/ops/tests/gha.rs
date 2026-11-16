//! `sb gha`'s sequence in process (ops spec, "`sb gha` (hidden)" and
//! success criterion 2): sb-ops' `gha::run` on the harness's stand-in
//! host and python-env's stand-in tree and fake GitHub, as any user. Each
//! part in sb-go's order, Python before the fact and the configs; a
//! failing part ends the run with its words, and nothing after it runs.
//! The binary's run as root, in the namespace on a symlinked root, is
//! crates/sb/tests/gha.rs.

use sb_cli_core::cancel::Shutdown;
use sb_ops::gha::{self, Gha};
use sb_ops::testing::Host;
use sb_python_env::testing::{FACT_TAG, GENERATION, PYTHON_RELEASE, Tree, fact_binary};
use sb_term_ui::{Options, Runner, Target};

/// Tests that write a stand-in program and run it hold this from writing
/// it until its run is over (ETXTBSY otherwise).
static SPAWNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// sb-go's six configs (`layout.Saltbox*ConfigPath`), written out here
/// rather than taken from the code under test.
const SIX: [&str; 6] = [
    "accounts.yml",
    "adv_settings.yml",
    "backup_config.yml",
    "hetzner_vlan.yml",
    "providers.yml",
    "settings.yml",
];

/// The stand-in host, and a tree whose checkout has a default for each
/// config and Saltbox's hooks script.
fn world() -> (Host, Tree) {
    let host = Host::new();
    let tree = Tree::new();
    let checkout = &tree.layout.checkout;
    std::fs::create_dir_all(checkout.join("defaults")).expect("defaults");
    for name in SIX {
        std::fs::write(
            checkout.join("defaults").join(format!("{name}.default")),
            format!("{name}: seed\n"),
        )
        .expect("default");
    }
    std::fs::create_dir_all(checkout.join("bin/git")).expect("bin/git");
    std::fs::write(checkout.join("bin/git/init-hooks"), "true\n").expect("hooks");
    (host, tree)
}

/// gha on `host` and `tree` as `sudo_user`: its result and the screen.
async fn run(host: &Host, tree: &Tree, sudo_user: Option<&str>) -> (bool, String) {
    let (apt, dirs, locale, bash) = (host.apt(), host.dirs(), host.locale(), host.bash());
    let python = tree.env();
    let gha = Gha {
        apt: &apt,
        dirs: &dirs,
        locale: &locale,
        bash: &bash,
        root: &tree.layout.checkout,
        python: &python,
        run_id: "run1",
        sudo_user,
    };
    let term = indicatif::InMemoryTerm::new(80, 400);
    let options = Options {
        target: Target::InMemory {
            term: term.clone(),
            colour: false,
        },
    };
    let shutdown = Shutdown::new();
    let mut runner = Runner::start("sb gha", &shutdown, None, options);
    let ran = gha::run(&mut runner, &gha).await;
    runner.finish();
    (ran.is_ok(), term.contents())
}

/// The labels' positions on the screen, which must rise.
fn in_order(screen: &str, labels: &[&str]) {
    let mut at = 0;
    for label in labels {
        let found = screen[at..]
            .find(label)
            .unwrap_or_else(|| panic!("{label} after {at}:\n{screen}"));
        at += found + label.len();
    }
}

#[tokio::test]
async fn gha_runs_every_part_in_sb_gos_order() {
    let _spawns = SPAWNS.lock().await;
    let (host, tree) = world();
    let _ = tree.healthy();
    let (ok, screen) = run(&host, &tree, None).await;
    assert!(ok, "{screen}");
    in_order(
        &screen,
        &[
            // The finished root step shows its done label.
            "GHA dependencies installed",
            "Updating apt package cache ",
            "Installing additional required packages",
            "Setting system-wide locale (LC_ALL and LANG)",
            "Ansible virtual environment",
            "saltbox.fact is ready",
            "Copying accounts.yml.default",
            "Copying settings.yml.default",
            "Activating Git hooks",
        ],
    );
    for name in SIX {
        assert!(tree.layout.checkout.join(name).is_file(), "{name}");
    }
}

/// A failing part, in each of gha's parts: the screen shows its failure,
/// and no later part runs.
#[tokio::test]
async fn a_failing_part_ends_gha_with_its_words() {
    let _spawns = SPAWNS.lock().await;
    // The prerequisites.
    let (host, tree) = world();
    let _ = tree.healthy();
    host.fail("apt-get");
    let (ok, screen) = run(&host, &tree, None).await;
    assert!(!ok);
    assert!(screen.contains("error updating apt cache"), "{screen}");
    assert!(!host.calls().iter().any(|c| c.starts_with("locale")));

    // The locale.
    let (host, tree) = world();
    let _ = tree.healthy();
    host.fail("locale-gen");
    let (ok, screen) = run(&host, &tree, None).await;
    assert!(!ok);
    assert!(screen.contains("error generating locale"), "{screen}");
    assert!(!screen.contains("Checking saltbox.fact"), "{screen}");

    // The venv, built from nothing, its `uv pip sync` failing (and its
    // one retry: python-env's decision 3).
    let (host, tree) = world();
    tree.fail("pip-sync");
    let (ok, screen) = run(&host, &tree, None).await;
    assert!(!ok);
    assert!(
        screen.contains("uv: pip-sync failed (stand-in)"),
        "{screen}"
    );
    assert!(!screen.contains("Checking saltbox.fact"), "{screen}");

    // The fact, with no release to read.
    let (host, tree) = world();
    let generation = tree.generation(GENERATION, PYTHON_RELEASE);
    tree.activate(&generation.venv);
    tree.fact(&fact_binary(FACT_TAG.trim_start_matches('v')));
    let (ok, screen) = run(&host, &tree, None).await;
    assert!(!ok);
    assert!(
        screen.contains("✗ Checking saltbox.fact") && screen.contains("returned HTTP 404"),
        "{screen}"
    );
    assert!(!tree.layout.checkout.join("accounts.yml").exists());

    // The configs: a link in accounts.yml's place, outside any sub-step,
    // so gha's own words lead the error.
    let (host, tree) = world();
    let _ = tree.healthy();
    let elsewhere = tree.root.path().join("elsewhere");
    std::fs::write(&elsewhere, "x\n").expect("elsewhere");
    std::os::unix::fs::symlink(&elsewhere, tree.layout.checkout.join("accounts.yml"))
        .expect("link");
    let (ok, screen) = run(&host, &tree, None).await;
    assert!(!ok);
    // term-ui shows the chain a cause a line: gha's words, then the
    // refusal.
    assert!(
        screen.contains("error copying default configuration files\n")
            && screen.contains("accounts.yml must not be a symlink"),
        "{screen}"
    );
    assert!(!host.calls().iter().any(|c| c.starts_with("bash")));

    // The hooks.
    let (host, tree) = world();
    let _ = tree.healthy();
    host.fail("bash");
    let (ok, screen) = run(&host, &tree, None).await;
    assert!(!ok);
    assert!(screen.contains("error activating Git hooks"), "{screen}");
}

/// An unknown or blank `SUDO_USER` skips the ownership (host's rule), and
/// gha still ends well.
#[tokio::test]
async fn an_unknown_sudo_user_skips_the_ownership() {
    let _spawns = SPAWNS.lock().await;
    for user in [" \n", "sb-rust-user-that-does-not-exist-7fcd24d1"] {
        let (host, tree) = world();
        let _ = tree.healthy();
        let (ok, screen) = run(&host, &tree, Some(user)).await;
        assert!(ok, "{user:?}:\n{screen}");
    }
}
