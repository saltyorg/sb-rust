//! `sb diag` through the binary with the fake `ansible-playbook`
//! (ansible spec, Testing strategy; module build M30): the argv it got,
//! element by element, its working directory, sb's lines, the run log,
//! and each ending passed on. `SB_ANSIBLE_PLAYBOOK` points at the fake
//! (crates/ansible/tests/fixtures/fake-ansible-playbook); no Ansible runs.
//! Root runs (`SB_TEST_ROOT=1`): sb logs, and needs root for the command.

mod common;
mod root_gate;

use std::path::PathBuf;
use std::process::{Command, Output};

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ansible/tests/fixtures/fake-ansible-playbook"
);

/// A test's world: a Saltbox root, a log directory and the fake's record
/// prefix.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        std::fs::create_dir(dir.path().join("saltbox")).expect("root");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        Self { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("saltbox")
    }

    fn logs(&self) -> PathBuf {
        self.dir.path().join("logs")
    }

    fn record(&self) -> PathBuf {
        self.dir.path().join("argv")
    }

    /// `sb <args>` in this world, with the fake as the playbook program.
    fn sb(&self, args: &[&str]) -> Command {
        let mut cmd = common::sb();
        cmd.args(args)
            .env("SB_SALTBOX_DIR", self.root())
            .env("SB_LOG_DIR", self.logs())
            .env("SB_ANSIBLE_PLAYBOOK", FAKE)
            .env("FAKE_ANSIBLE_LOG", self.record())
            .env_remove("FAKE_ANSIBLE_EXIT")
            .env_remove("FAKE_ANSIBLE_MODE");
        cmd
    }

    /// Each run's record: its working directory, `$0`, its arguments.
    fn runs(&self) -> Vec<Vec<String>> {
        let mut runs = Vec::new();
        for n in 0.. {
            let path = PathBuf::from(format!("{}.{n}", self.record().display()));
            let Ok(bytes) = std::fs::read(&path) else {
                break;
            };
            let text = String::from_utf8(bytes).expect("UTF-8");
            let mut elements: Vec<String> = text.split('\0').map(str::to_string).collect();
            assert_eq!(elements.pop().as_deref(), Some(""), "NUL-terminated");
            runs.push(elements);
        }
        runs
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.logs().join("sb.log")).unwrap_or_default()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `sb diag`: `<program> saltbox.yml --become --tags diag` in the Saltbox
/// root, attached, with sb-go's lines on stdout and the masked command in
/// the run log.
#[test]
fn diag_runs_the_diag_tag_in_the_saltbox_repo() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world.sb(&["diag"]).output().expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let root = world.root().display().to_string();
    assert_eq!(
        world.runs(),
        [vec![
            root.clone(),
            FAKE.to_string(),
            "saltbox.yml".into(),
            "--become".into(),
            "--tags".into(),
            "diag".into(),
        ]]
    );
    assert_eq!(
        stdout(&output),
        format!(
            "Executing Ansible playbook with command: {FAKE} saltbox.yml --become --tags diag\n\
             \nPlaybook {root}/saltbox.yml executed successfully.\n"
        )
    );
    let log = world.log();
    assert!(log.contains("child started"), "{log}");
    assert!(
        log.contains(&format!("{FAKE} saltbox.yml --become --tags diag")),
        "{log}"
    );
    assert!(
        log.contains("way=\"attached\"") || log.contains("way=attached"),
        "{log}"
    );
    assert!(log.contains("playbook program overridden"), "{log}");
}

/// A failed playbook: sb exits with Ansible's own code, exec's block names
/// the command and its code, then sb-go's hint; 99 (Ansible's Ctrl-C) says
/// Ansible was interrupted. No success line.
#[test]
fn a_failed_playbook_passes_its_code_through() {
    if !root_gate::requested() {
        return;
    }
    for (code, hint) in [
        (2, "Scroll up to the failed task to review."),
        (4, "Scroll up to the failed task to review."),
        (250, "Scroll up to the failed task to review."),
        (99, "Ansible was interrupted."),
    ] {
        let world = World::new();
        let output = world
            .sb(&["diag"])
            .env("FAKE_ANSIBLE_EXIT", code.to_string())
            .output()
            .expect("run sb");
        assert_eq!(output.status.code(), Some(code), "{}", stderr(&output));
        let err = console::strip_ansi_codes(&stderr(&output)).into_owned();
        assert!(
            err.contains(&format!(
                "{FAKE} saltbox.yml --become --tags diag exited with code {code}"
            )),
            "{code}: {err}"
        );
        assert!(err.trim_end().ends_with(hint), "{code}: {err}");
        assert!(!stdout(&output).contains("executed successfully"));
        assert_eq!(world.runs().len(), 1);
    }
}

/// A playbook killed by a signal sb never got (the OOM killer's
/// SIGKILL): sb exits 137, with exec's block and sb-go's hint.
#[test]
fn a_playbook_killed_on_its_own_exits_128_plus_the_signal() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb(&["diag"])
        .env("FAKE_ANSIBLE_MODE", "self-kill")
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(137), "{}", stderr(&output));
    let err = console::strip_ansi_codes(&stderr(&output)).into_owned();
    assert!(err.contains("was killed by SIGKILL"), "{err}");
    assert!(
        err.trim_end()
            .ends_with("Scroll up to the failed task to review."),
        "{err}"
    );
}

/// A playbook program that can't start is sb's error block, exit 1.
#[test]
fn a_missing_program_is_a_spawn_error() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let missing = world.dir.path().join("no-such-playbook");
    let output = world
        .sb(&["diag"])
        .env("SB_ANSIBLE_PLAYBOOK", &missing)
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains(&format!("could not run {}", missing.display())),
        "{}",
        stderr(&output)
    );
}

/// `diag` takes no arguments (sb-go's `cobra.NoArgs`): clap's usage
/// error, exit 2, nothing run.
#[test]
fn diag_takes_no_arguments() {
    let world = World::new();
    let output = world.sb(&["diag", "extra"]).output().expect("run sb");
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(world.runs(), Vec::<Vec<String>>::new());
}
