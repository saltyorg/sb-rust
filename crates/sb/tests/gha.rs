//! `sb gha` through the built binary (ops spec, "`sb gha` (hidden)" and
//! Testing Strategy, "Sequences"): as root (`SB_TEST_ROOT=1`) in sb-ops'
//! private mount namespace, where `/etc/apt`, `/var/backups`,
//! `/etc/default` and `/srv` are empty tmpfs and stand-in apt-get,
//! `lsb_release`, locale programs and bash come first on `PATH`; the Saltbox
//! root is `/srv/git/saltbox`, a symlink to python-env's stand-in
//! checkout, as Saltbox's CI links its workspace there; the venv and
//! saltbox.fact on python-env's stand-in tree and the fake GitHub.
//! `SUDO_USER` names the configs' owner, as after sb's relaunch. Nothing
//! reaches the host's apt, sources, locale or `/srv`; the release client
//! of the test build talks only to the fake GitHub on loopback (the
//! namespace keeps the host's network for it).

mod common;
mod root_gate;

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::Output;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use sb_ops::testing::Host;
use sb_python_env::testing::{ROOT_VAR, Tree};
use sb_release::testing::BASES_VARIABLE;

/// The Saltbox root as gha sees it, inside the namespace.
const ROOT: &str = "/srv/git/saltbox";

/// sb-go's six configs (`layout.Saltbox*ConfigPath`), written out here
/// rather than taken from the code under test.
const CONFIGS: [&str; 6] = [
    "accounts.yml",
    "adv_settings.yml",
    "backup_config.yml",
    "hetzner_vlan.yml",
    "providers.yml",
    "settings.yml",
];

/// A stand-in host and Saltbox tree: the checkout with a default for
/// each of gha's six configs and Saltbox's hooks script; `update-locale`
/// writing the namespace's own locale file.
fn world() -> (Host, Tree) {
    let host = Host::new();
    let tree = Tree::new();
    let _ = tree.healthy();
    let checkout = &tree.layout.checkout;
    std::fs::create_dir_all(checkout.join("defaults")).expect("defaults");
    for name in CONFIGS {
        std::fs::write(
            checkout.join("defaults").join(format!("{name}.default")),
            format!("{name}: seed\n"),
        )
        .expect("default");
    }
    std::fs::create_dir_all(checkout.join("bin/git")).expect("bin/git");
    std::fs::write(checkout.join("bin/git/init-hooks"), "true\n").expect("hooks");
    let _ = host.stand_in(
        "update-locale",
        "printf 'LANG=en_US.UTF-8\\nLC_ALL=en_US.UTF-8\\n' > /etc/default/locale",
    );
    // The hooks' working directory, kept beside the call log.
    let cwd = host.path("hooks-cwd");
    let _ = host.stand_in("bash", &format!("pwd > '{}'", cwd.display()));
    (host, tree)
}

/// `sb gha` in the namespace, `SUDO_USER` set to `sudo_user`.
fn gha(host: &Host, tree: &Tree, sudo_user: &str) -> Output {
    let sb = PathBuf::from(common::sb().get_program());
    let setup = format!(
        "mkdir -p /etc/apt/sources.list.d /srv/git\nln -s '{}' {ROOT}",
        tree.layout.checkout.display()
    );
    let mut cmd = host.namespaced(&sb, &["gha", "--color=never"], &setup);
    common::apply_sb_env(&mut cmd);
    cmd.env(ROOT_VAR, tree.root.path())
        .env(BASES_VARIABLE, tree.bases())
        .env("SB_SALTBOX_DIR", ROOT)
        .env("SUDO_USER", sudo_user)
        .output()
        .expect("run sb gha")
}

#[test]
fn gha_is_hidden_and_takes_nothing() {
    common::sb()
        .args(["--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains(" gha ").not());
    common::sb()
        .args(["gha", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Install GHA dependencies"));
    common::sb().args(["gha", "extra"]).assert().code(2);
    common::sb().args(["gha", "-v"]).assert().code(2);
}

/// sb-go's sequence on a symlinked root, the six configs the sudo user's
/// with 0600, the hooks run in the root.
#[test]
fn gha_runs_sb_gos_sequence_on_a_symlinked_root() {
    if !root_gate::requested() {
        return;
    }
    let (host, tree) = world();
    let out = gha(&host, &tree, " nobody\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // Each part in sb-go's order: Python before the fact and the configs.
    let mut at = 0;
    for line in [
        "Installing GHA dependencies",
        "Updating apt package cache",
        "Generating locale en_US.UTF-8",
        "Ansible virtual environment",
        "saltbox.fact is ready",
        "Copying accounts.yml.default",
        "Activating Git hooks",
        "GHA dependencies installed",
    ] {
        let found = stderr[at..]
            .find(line)
            .unwrap_or_else(|| panic!("{line} after {at}:\n{stderr}"));
        at += found + line.len();
    }
    // The programs, in sb-go's order: apt's sequence, the locale, the
    // hooks last.
    let programs: Vec<String> = host
        .calls()
        .iter()
        .map(|call| call.split(' ').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        programs,
        [
            "apt-get",
            "apt-get",
            "apt-get",
            "lsb_release",
            "apt-get",
            "apt-get",
            "locale",
            "locale-gen",
            "update-locale",
            "bash"
        ],
        "{:?}",
        host.calls()
    );
    assert_eq!(
        host.calls().last().map(String::as_str),
        Some(format!("bash {ROOT}/bin/git/init-hooks").as_str())
    );
    // ...run in the root (the link, or where it leads).
    let cwd = std::fs::read_to_string(host.path("hooks-cwd")).expect("hooks cwd");
    let checkout = tree.layout.checkout.display().to_string();
    assert!([ROOT, checkout.as_str()].contains(&cwd.trim_end()), "{cwd}");
    // The configs are the sudo user's (`SUDO_USER` trimmed): nobody's uid
    // on Ubuntu (base-passwd), 0600, each the default's content.
    for name in CONFIGS {
        let path = tree.layout.checkout.join(name);
        let meta = std::fs::symlink_metadata(&path).expect("config");
        assert_eq!((meta.uid(), meta.gid()), (65534, 65534), "{name}");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "{name}");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            format!("{name}: seed\n")
        );
    }
}

/// No `SUDO_USER`: the configs stay root's; a failing apt ends gha with
/// its words, before anything else runs.
#[test]
fn gha_without_a_sudo_user_and_with_a_failing_apt() {
    if !root_gate::requested() {
        return;
    }
    let (host, tree) = world();
    let out = gha(&host, &tree, "");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let accounts = tree.layout.checkout.join("accounts.yml");
    assert_eq!(std::fs::metadata(&accounts).expect("accounts").uid(), 0);

    let (host, tree) = world();
    host.fail("apt-get");
    let out = gha(&host, &tree, "nobody");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("error updating apt cache"), "{stderr}");
    assert!(
        host.calls().iter().all(|call| !call.starts_with("locale")),
        "{:?}",
        host.calls()
    );
    assert!(!tree.layout.checkout.join("accounts.yml").exists());
}
