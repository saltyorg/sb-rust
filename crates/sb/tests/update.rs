//! `sb update`'s first half through the built binary (ops spec, "`sb
//! update`" and Testing Strategy, "Commands end to end"; module build
//! M84): its flags as any run; the runs as root (`SB_TEST_ROOT=1`) in
//! sb-ops' private mount namespace, where `/etc/apt`, `/var/backups`,
//! `/etc/default` and `/srv` are empty tmpfs and a stand-in apt-get comes
//! first on `PATH`, each in a session of its own (util-linux's `setsid`),
//! so there is no terminal. The Saltbox checkout (`SB_SALTBOX_DIR`) and
//! the Sandbox checkout under its inventory's appdata path are clones of
//! sb-ops' stand-in remotes in a temporary directory (`file://`, never
//! the network); the test build is raw, so self-update is off, and its
//! release client talks only to a fake GitHub with nothing scripted.

mod common;
mod raw_pty;
mod root_gate;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use raw_pty::Pty;
use sb_ops::testing::{Host, Remote, git};
use sb_release::testing::{BASES_VARIABLE, Fake};

/// A stand-in host; the Saltbox and Sandbox checkouts, clones of their
/// own remotes at a branch each; the inventory pointing the appdata path
/// into the temporary directory; accounts.yml naming nobody; the fake
/// GitHub's bases.
struct World {
    host: Host,
    dir: tempfile::TempDir,
    remotes: (Remote, Remote),
    _fake: Fake,
    bases: PathBuf,
}

impl World {
    fn new(saltbox: &str, sandbox: &str) -> Self {
        let host = Host::new();
        let dir = tempfile::tempdir().expect("tempdir");
        let remotes = (
            Remote::saltbox(&[], "nobody"),
            Remote::saltbox(&[], "nobody"),
        );
        let fake = Fake::start().expect("fake GitHub");
        let bases = dir.path().join("bases.json");
        fake.write_bases(&bases).expect("bases");
        let world = Self {
            host,
            dir,
            remotes,
            _fake: fake,
            bases,
        };
        world.remotes.0.clone_into(saltbox, &world.saltbox());
        world.remotes.1.clone_into(sandbox, &world.sandbox());
        let inventory = world.saltbox().join("inventories/host_vars");
        std::fs::create_dir_all(&inventory).expect("inventory dir");
        std::fs::write(
            inventory.join("localhost.yml"),
            format!("server_appdata_path: {}\n", world.appdata().display()),
        )
        .expect("inventory");
        std::fs::write(
            world.saltbox().join("accounts.yml"),
            "user:\n  name: nobody\n",
        )
        .expect("accounts");
        world
    }

    fn saltbox(&self) -> PathBuf {
        self.dir.path().join("saltbox")
    }

    fn appdata(&self) -> PathBuf {
        self.dir.path().join("appdata")
    }

    fn sandbox(&self) -> PathBuf {
        self.appdata().join("sandbox")
    }

    /// A checkout's branch and `branch.txt`.
    fn branch(path: &Path) -> (String, String) {
        let name = git(path, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let text = std::fs::read_to_string(path.join("branch.txt")).expect("branch.txt");
        (name, text.trim().to_owned())
    }

    /// `sb update <args>` in the namespace and a session of its own: no
    /// terminal.
    fn update(&self, args: &[&str]) -> Output {
        let sb = common::sb().get_program().to_string_lossy().into_owned();
        let mut command = vec!["-w", sb.as_str(), "update", "--color=never"];
        command.extend_from_slice(args);
        self.namespaced(Path::new("/usr/bin/setsid"), &command)
            .output()
            .expect("run sb update")
    }

    /// `program <args>` in the namespace, with sb's test environment and
    /// the world's.
    fn namespaced(&self, program: &Path, args: &[&str]) -> Command {
        let mut cmd = self
            .host
            .namespaced(program, args, "mkdir -p /etc/apt/sources.list.d");
        common::apply_sb_env(&mut cmd);
        cmd.env("SB_SALTBOX_DIR", self.saltbox())
            .env(BASES_VARIABLE, &self.bases)
            .env("SB_LOG_DIR", self.dir.path().join("logs"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            // The checkouts are the Saltbox user's after a run; Saltbox
            // adds them to root's global safe.directory (repo spec,
            // assumption 2), the environment's config stands in for it.
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "safe.directory")
            .env("GIT_CONFIG_VALUE_0", "*");
        cmd
    }
}

/// The lines' positions in `text`, which must rise.
fn in_order(text: &str, lines: &[&str]) {
    let mut at = 0;
    for line in lines {
        let found = text[at..]
            .find(line)
            .unwrap_or_else(|| panic!("{line} after {at}:\n{text}"));
        at += found + line.len();
    }
}

#[test]
fn update_is_shown_with_sb_gos_flags_and_migrations() {
    common::sb()
        .args(["--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Update Saltbox & Sandbox"));
    common::sb()
        .args(["update", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("--keep-branch")
                .and(predicate::str::contains("--reset-branch"))
                .and(predicate::str::contains("--skip-self-update"))
                .and(predicate::str::contains("--migrations"))
                .and(predicate::str::contains("--verbose").not()),
        );
    common::sb()
        .args(["update", "--keep-branch", "--reset-branch"])
        .assert()
        .code(2);
    common::sb()
        .args(["update", "--migrations", "maybe"])
        .assert()
        .code(2);
    common::sb().args(["update", "-v"]).assert().code(2);
}

/// Piped with every flag: self-update off (a raw build), apt, then Saltbox
/// and Sandbox reset to master and given to the Saltbox user.
#[test]
fn piped_with_every_flag_both_checkouts_update() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "develop");
    let out = world.update(&["--reset-branch", "--migrations", "skip"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    in_order(
        &stderr,
        &[
            "Self-update is disabled in this build",
            "Updating apt package cache",
            "Updating Saltbox",
            "Saltbox repository updated (master)",
            "Updating Sandbox",
            "Sandbox repository updated (master)",
        ],
    );
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
    assert_eq!(
        World::branch(&world.sandbox()),
        ("master".into(), "master".into())
    );
    let calls = world.host.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].starts_with("apt-get ") && calls[0].ends_with(" update"),
        "{calls:?}"
    );
}

/// Without a terminal: no --migrations, or a checkout off master and no
/// branch flag, is refused before any step, naming each flag; nothing
/// ran and nothing changed.
#[test]
fn without_a_terminal_missing_flags_are_refused_first() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("master", "develop");
    let out = world.update(&["--skip-self-update"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    for text in [
        "--keep-branch or --reset-branch (Sandbox is on branch 'develop', not 'master')",
        "--migrations run or --migrations skip",
    ] {
        assert!(stderr.contains(text), "{text}: {stderr}");
    }
    assert!(!stderr.contains("Updating"), "{stderr}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    assert_eq!(
        World::branch(&world.sandbox()),
        ("develop".into(), "develop".into())
    );

    let world = World::new("master", "master");
    let out = world.update(&["--reset-branch"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("--migrations run or --migrations skip"),
        "{stderr}"
    );
    assert!(!stderr.contains("--keep-branch"), "{stderr}");
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
}

/// On a terminal with both checkouts off master (ops spec, Testing
/// Strategy: "`update` on a PTY: the branch questions before the first
/// step"): Saltbox's question, then Sandbox's, each after its current
/// branch, on the terminal (term-ui's confirm), and only then apt and the
/// checkouts; `y` resets Saltbox, `n` keeps Sandbox's branch.
#[test]
fn on_a_terminal_both_questions_come_before_the_first_step() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new("develop", "develop");
    let sb = common::sb().get_program().to_string_lossy().into_owned();
    let cmd = world.namespaced(
        Path::new(&sb),
        &["update", "--color=never", "--migrations", "skip"],
    );
    let mut pty = Pty::spawn(&cmd);
    let saltbox = "Saltbox: Do you want to reset to the 'master' branch?";
    assert!(pty.showing(saltbox), "{}", pty.text());
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    pty.send(b"y\r");
    let sandbox = "Sandbox: Do you want to reset to the 'master' branch?";
    assert!(pty.showing(sandbox), "{}", pty.text());
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    pty.send(b"n\r");
    let status = pty.exit();
    let text = pty.text();
    assert!(status.success(), "{text}");
    in_order(
        &text,
        &[
            "Saltbox: Currently on branch 'develop'",
            saltbox,
            "Sandbox: Currently on branch 'develop'",
            sandbox,
            "Updating apt package cache",
            "Saltbox repository updated (master)",
            "Sandbox: Updating the current branch 'develop'",
            "Sandbox repository updated (develop)",
        ],
    );
    assert_eq!(
        World::branch(&world.saltbox()),
        ("master".into(), "master".into())
    );
    assert_eq!(
        World::branch(&world.sandbox()),
        ("develop".into(), "develop".into())
    );
}
