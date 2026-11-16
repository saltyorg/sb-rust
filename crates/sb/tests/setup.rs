//! `sb setup` through the built binary (ops spec, "`sb setup` (hidden)"
//! and Testing Strategy, "setup guard", "Sequences" and "setup's branch
//! flags"): its flags as any run; the runs as root (`SB_TEST_ROOT=1`) in
//! sb-ops' private mount namespace, where `/etc/apt`, `/var/backups`,
//! `/etc/default` and `/srv` are empty tmpfs, `/etc/os-release` names a
//! release setup supports, and stand-in apt-get, `lsb_release`, locale
//! programs, `systemd-detect-virt`, `dpkg-query` and bash come first on
//! `PATH`. Each run is in a session of its own (util-linux's `setsid`),
//! so there is no terminal to ask on. The Saltbox root is python-env's
//! stand-in checkout (`SB_SALTBOX_DIR`), cloned from sb-ops' stand-in
//! remote: git's `url.<base>.insteadOf`, given through
//! `GIT_CONFIG_COUNT` (<https://git-scm.com/docs/git-config#ENVIRONMENT>),
//! sends Saltbox's URL to it, so no run reaches the network, and stands
//! in for Saltbox's safe.directory entry for the checkout. The venv
//! and saltbox.fact on python-env's stand-in tree and the fake GitHub on
//! loopback (the namespace keeps the host's network for it).

mod common;
mod root_gate;

use std::io::{Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use sb_ops::testing::{Host, Remote, git};
use sb_python_env::testing::{ROOT_VAR, Tree};
use sb_release::testing::BASES_VARIABLE;

/// Saltbox's repository as setup clones it, written out here rather than
/// taken from the code under test.
const SALTBOX_URL: &str = "https://github.com/saltyorg/saltbox.git";

/// A stand-in host, a python-env tree with no checkout yet (a fresh
/// server), and the stand-in remote holding the tree's pins and lock,
/// accounts.yml's default naming nobody.
struct World {
    host: Host,
    tree: Tree,
    remote: Remote,
}

impl World {
    fn new() -> Self {
        let host = Host::new();
        let tree = Tree::new();
        let _ = tree.healthy();
        let layout = &tree.layout;
        let pins: Vec<(String, String)> = [&layout.python_pin, &layout.uv_pin, &layout.lock]
            .iter()
            .map(|pin| {
                let rel = pin.strip_prefix(&layout.checkout).expect("in the checkout");
                let text = std::fs::read_to_string(pin).expect("pin");
                (rel.display().to_string(), text)
            })
            .collect();
        let files: Vec<(&str, &str)> = pins.iter().map(|(r, t)| (r.as_str(), t.as_str())).collect();
        let remote = Remote::saltbox(&files, "nobody");
        std::fs::remove_dir_all(&layout.checkout).expect("remove the tree's checkout");
        // A supported release, bound over the host's inside the namespace.
        std::fs::write(
            host.path("os-release"),
            "ID=ubuntu\nVERSION_ID=\"24.04\"\nVERSION_CODENAME=noble\n",
        )
        .expect("os-release");
        let _ = host.stand_in(
            "update-locale",
            "printf 'LANG=en_US.UTF-8\\nLC_ALL=en_US.UTF-8\\n' > /etc/default/locale",
        );
        // The hooks' working directory, kept beside the call log.
        let cwd = host.path("hooks-cwd");
        let _ = host.stand_in("bash", &format!("pwd > '{}'", cwd.display()));
        Self { host, tree, remote }
    }

    fn root(&self) -> &Path {
        &self.tree.layout.checkout
    }

    /// The root's branch and `branch.txt`.
    fn branch(&self) -> (String, String) {
        let name = git(self.root(), &["rev-parse", "--abbrev-ref", "HEAD"]);
        let text = std::fs::read_to_string(self.root().join("branch.txt")).expect("branch.txt");
        (name, text.trim().to_owned())
    }

    /// The run's log directory.
    fn logs(&self) -> PathBuf {
        self.host.path("logs")
    }

    /// `sb setup <args>` in the namespace and a session of its own: no
    /// terminal.
    fn setup(&self, args: &[&str]) -> Output {
        let sb = common::sb().get_program().to_string_lossy().into_owned();
        let mut command = vec!["-w", sb.as_str(), "setup", "--color=never"];
        command.extend_from_slice(args);
        self.namespaced(Path::new("/usr/bin/setsid"), &command)
            .output()
            .expect("run sb setup")
    }

    /// `program <args>` in the namespace, with sb's test environment and
    /// the world's.
    fn namespaced(&self, program: &Path, args: &[&str]) -> Command {
        let prepare = format!(
            "mkdir -p /etc/apt/sources.list.d\nmount --bind '{}' /etc/os-release",
            self.host.path("os-release").display()
        );
        let mut cmd = self.host.namespaced(program, args, &prepare);
        common::apply_sb_env(&mut cmd);
        cmd.env(ROOT_VAR, self.tree.root.path())
            .env(BASES_VARIABLE, self.tree.bases())
            .env("SB_SALTBOX_DIR", self.root())
            .env("SB_LOG_DIR", self.logs())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_COUNT", "2")
            .env(
                "GIT_CONFIG_KEY_0",
                format!("url.{}.insteadOf", self.remote.url),
            )
            .env("GIT_CONFIG_VALUE_0", SALTBOX_URL)
            // A setup run gives the checkout to the Saltbox user, and
            // Saltbox adds it to root's global safe.directory (repo spec,
            // assumption 2); the environment's config stands in for that
            // entry here.
            .env("GIT_CONFIG_KEY_1", "safe.directory")
            .env("GIT_CONFIG_VALUE_1", self.root());
        cmd
    }

    /// The programs the stand-ins saw, in order.
    fn programs(&self) -> Vec<String> {
        self.host
            .calls()
            .iter()
            .map(|call| call.split(' ').next().unwrap_or_default().to_owned())
            .collect()
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
fn setup_is_hidden_with_sb_gos_flags_and_the_branch_ones() {
    common::sb()
        .args(["--help", "--color=never"])
        .assert()
        .success()
        .stdout(predicate::str::contains(" setup ").not());
    common::sb()
        .args(["setup", "--help", "--color=never"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Install Saltbox and its dependencies")
                .and(predicate::str::contains("--branch"))
                .and(predicate::str::contains("--force"))
                .and(predicate::str::contains("--keep-branch"))
                .and(predicate::str::contains("--reset-branch"))
                .and(predicate::str::contains("--verbose").not()),
        );
    common::sb().args(["setup", "extra"]).assert().code(2);
    common::sb()
        .args(["setup", "--keep-branch", "--reset-branch"])
        .assert()
        .code(2);
}

/// The guard's refusals through the binary: before any step, nothing run.
#[test]
fn the_guard_refuses_before_any_step() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let root = world.root().display().to_string();
    // A directory without --force.
    std::fs::create_dir_all(world.root()).expect("root");
    let out = world.setup(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "setup already exists at {root}; setup is intended"
        )),
        "{stderr}"
    );
    // A symlink, even with --force.
    std::fs::remove_dir(world.root()).expect("rmdir");
    std::os::unix::fs::symlink(world.tree.root.path(), world.root()).expect("symlink");
    let out = world.setup(&["--force"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("{root} exists but is a symlink")),
        "{stderr}"
    );
    // A file, even with --force.
    std::fs::remove_file(world.root()).expect("rm");
    std::fs::write(world.root(), "").expect("file");
    let out = world.setup(&["--force"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("{root} exists but is not a directory")),
        "{stderr}"
    );
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
}

/// A fresh install: sb-go's phases and programs in order, the clone at
/// `-b`'s branch, the configs 0600, the hooks in the root, `-v` only a
/// run-log line.
#[test]
fn a_fresh_install_runs_sb_gos_sequence() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let out = world.setup(&["-b", "develop", "-v"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    in_order(
        &stderr,
        &[
            "Installing Saltbox",
            "Checking system compatibility",
            "Checking for desktop environment",
            "Installing system prerequisites",
            "Updating apt package cache",
            "Configuring system locale",
            "Preparing Saltbox repository",
            &format!(
                "Cloning Saltbox repository to {} (branch: develop)",
                world.root().display()
            ),
            "saltbox.fact is ready",
            "Copying accounts.yml.default",
            "Activating Git hooks",
            "Installing Python runtime",
            "Ansible virtual environment",
            "Saltbox installation completed",
        ],
    );
    assert_eq!(
        world.programs(),
        [
            "systemd-detect-virt",
            "dpkg-query",
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
        world.host.calls()
    );
    assert_eq!(world.branch(), ("develop".to_owned(), "develop".to_owned()));
    let cwd = std::fs::read_to_string(world.host.path("hooks-cwd")).expect("hooks cwd");
    assert_eq!(cwd.trim_end(), world.root().display().to_string());
    for name in ["accounts.yml", "settings.yml"] {
        let meta = std::fs::symlink_metadata(world.root().join(name)).expect(name);
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "{name}");
    }
    // `-v` changed nothing on screen; the run log says it was ignored.
    assert!(!stderr.contains("verbose"), "{stderr}");
    let logged = std::fs::read_dir(world.logs())
        .expect("logs")
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .any(|text| text.contains("--verbose given"));
    assert!(logged, "no run-log line for -v");
}

/// `--force` over a checkout on another branch: `--reset-branch` resets
/// to `-b`'s, `--keep-branch` updates the current one with repo's
/// notices; neither, with no terminal, fails before any step naming both
/// flags.
#[test]
fn the_branch_flags_answer_over_an_existing_checkout() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.remote.clone_into("develop", world.root());
    let out = world.setup(&["--force"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("Do you want to reset to the 'master' branch?")
            && stderr.contains("--keep-branch or --reset-branch"),
        "{stderr}"
    );
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());

    let out = world.setup(&["--force", "--keep-branch"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    in_order(
        &stderr,
        &[
            "Saltbox: Currently on branch 'develop'",
            "Saltbox: Updating the current branch 'develop'",
            "Saltbox repository updated (develop)",
        ],
    );
    assert_eq!(world.branch(), ("develop".to_owned(), "develop".to_owned()));

    let out = world.setup(&["--force", "--reset-branch"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("Saltbox repository updated (master)"),
        "{stderr}"
    );
    assert!(!stderr.contains("Updating the current branch"), "{stderr}");
    assert_eq!(world.branch(), ("master".to_owned(), "master".to_owned()));
    // The fetch gives the checkout to root (sb-go's), then the Python
    // phase to the Saltbox user: nobody's uid on Ubuntu (base-passwd).
    let owner = std::fs::metadata(world.root().join("branch.txt")).expect("stat");
    assert_eq!(owner.uid(), 65534);
}

/// A command on a PTY of its own (portable-pty, as `logs_pty`'s): its
/// output gathered as it arrives, raw.
struct Pty {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // The master kept open while the child runs.
    _master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    output: std::sync::mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
}

impl Pty {
    /// How long a test waits for a screen or an exit.
    const WAIT: Duration = Duration::from_secs(60);

    /// `cmd` (its program, arguments and environment) on a new PTY.
    fn spawn(cmd: &Command) -> Self {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        let mut builder = CommandBuilder::new(cmd.get_program());
        builder.args(cmd.get_args());
        builder.env("TERM", "xterm-256color");
        for (name, value) in cmd.get_envs() {
            match value {
                Some(value) => builder.env(name, value),
                None => builder.env_remove(name),
            }
        }
        let child = pair.slave.spawn_command(builder).expect("spawn");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer = pair.master.take_writer().expect("writer");
        let (tx, output) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0; 4096];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            _master: pair.master,
            writer,
            output,
            seen: Vec::new(),
        }
    }

    /// Everything the command wrote so far, as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    /// Wait until the output holds `text`; false at the timeout.
    fn showing(&mut self, text: &str) -> bool {
        let deadline = Instant::now() + Self::WAIT;
        while !self.text().contains(text) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.output.recv_timeout(left) {
                Ok(bytes) => self.seen.extend(bytes),
                Err(_) => return self.text().contains(text),
            }
        }
        true
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write");
        self.writer.flush().expect("flush");
    }

    /// Wait for the command to exit, gathering its output meanwhile.
    fn exit(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + Self::WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                while let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(200)) {
                    self.seen.extend(bytes);
                }
                return status;
            }
            assert!(Instant::now() < deadline, "no exit:\n{}", self.text());
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
                self.seen.extend(bytes);
            }
        }
    }
}

/// At a terminal, over a checkout on another branch: sb-go's line for the
/// current branch, then the question on the terminal (term-ui's confirm);
/// `n` keeps the branch, noted once the run starts.
#[test]
fn at_a_terminal_the_question_follows_the_current_branch() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.remote.clone_into("develop", world.root());
    let sb = common::sb().get_program().to_string_lossy().into_owned();
    let cmd = world.namespaced(Path::new(&sb), &["setup", "--color=never", "--force"]);
    let mut pty = Pty::spawn(&cmd);
    let question = "Saltbox: Do you want to reset to the 'master' branch?";
    assert!(pty.showing(question), "{}", pty.text());
    let text = pty.text();
    let current = text
        .find("Saltbox: Currently on branch 'develop'")
        .unwrap_or_else(|| panic!("no current branch line:\n{text}"));
    assert!(current < text.find(question).unwrap_or_default(), "{text}");
    // Nothing ran before the answer.
    assert!(world.host.calls().is_empty(), "{:?}", world.host.calls());
    pty.send(b"n\r");
    let status = pty.exit();
    assert!(status.success(), "{}", pty.text());
    let text = pty.text();
    assert!(
        text.contains("Saltbox: Updating the current branch 'develop'"),
        "{text}"
    );
    // The current branch line once, before the question only.
    assert_eq!(
        text.matches("Currently on branch 'develop'").count(),
        1,
        "{text}"
    );
    assert_eq!(world.branch(), ("develop".to_owned(), "develop".to_owned()));
}
