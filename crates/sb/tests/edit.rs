//! `sb edit <file>` through the binary with a fake editor (tui spec,
//! "Editors", Testing strategy; module build M49): a shell script that
//! records its name and arguments, can append bytes to the file, exits
//! with a chosen code, or waits for a signal. Every test uses its own
//! Saltbox root, log directory and `PATH`; none opens a real editor.

mod common;
mod root_gate;

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};

/// The fake editor: `$0` and each argument on its own line to
/// `$FAKE_ARGV`, then `$FAKE_APPEND` appended to its last argument (the
/// file), then exit `$FAKE_EXIT`. With `FAKE_WAIT` set it writes its pid
/// to `$FAKE_ARGV.pid` and waits; a SIGTERM writes `$FAKE_ARGV.term` and
/// ends it.
const FAKE: &str = r#"#!/bin/sh
printf '%s\n' "$0" "$@" > "$FAKE_ARGV"
for last; do :; done
if [ -n "$FAKE_APPEND" ]; then printf '%s' "$FAKE_APPEND" >> "$last"; fi
if [ -n "$FAKE_WAIT" ]; then
  trap 'echo term > "$FAKE_ARGV.term"; exit 0' TERM
  echo $$ > "$FAKE_ARGV.pid"
  while :; do /bin/sleep 0.05; done
fi
exit "${FAKE_EXIT:-0}"
"#;

/// The six files and their subcommands, as sb-go's layout has them.
const FILES: [(&str, &str); 6] = [
    ("accounts", "accounts.yml"),
    ("settings", "settings.yml"),
    ("adv_settings", "adv_settings.yml"),
    ("backup_config", "backup_config.yml"),
    ("hetzner_vlan", "hetzner_vlan.yml"),
    ("inventory", "inventories/host_vars/localhost.yml"),
];

/// A test's world: a Saltbox root holding the six files, a `bin`
/// directory for `PATH`, the fake editor in it, and a log directory.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        let root = dir.path().join("saltbox");
        std::fs::create_dir_all(root.join("inventories/host_vars")).expect("root");
        for (_, file) in FILES {
            std::fs::write(root.join(file), "a: 1\n").expect("file");
        }
        std::fs::create_dir(dir.path().join("bin")).expect("bin");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        let world = Self { dir };
        world.stub("fake-editor");
        world
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("saltbox")
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    fn fake(&self) -> String {
        self.bin().join("fake-editor").display().to_string()
    }

    fn argv_file(&self) -> PathBuf {
        self.dir.path().join("argv")
    }

    fn logs(&self) -> PathBuf {
        self.dir.path().join("logs")
    }

    /// The fake editor installed in `bin` as `name`.
    fn stub(&self, name: &str) {
        let path = self.bin().join(name);
        std::fs::write(&path, FAKE).expect("stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    /// `sb edit <sub>` in this world, with no editor variables, `PATH`
    /// holding only `bin`, and the fake's settings.
    fn sb(&self, sub: &str) -> Command {
        let mut cmd = common::sb();
        cmd.args(["edit", sub])
            .env("SB_SALTBOX_DIR", self.root())
            .env("SB_LOG_DIR", self.logs())
            .env("PATH", self.bin())
            .env("FAKE_ARGV", self.argv_file())
            .env_remove("VISUAL")
            .env_remove("EDITOR");
        cmd
    }

    /// What the fake recorded: its `$0`, then its arguments.
    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.argv_file())
            .expect("the fake ran")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn ran(&self) -> bool {
        self.argv_file().exists()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.logs().join("sb.log")).unwrap_or_default()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Each subcommand opens its file: the editor gets `VISUAL`'s extra words,
/// quotes and backslashes taken as a shell takes them, then the path.
#[test]
fn each_subcommand_opens_its_file_with_the_extra_words() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    for (sub, file) in FILES {
        let output = world
            .sb(sub)
            .env("VISUAL", format!(r#"{} --wait "-c x" a\ b"#, world.fake()))
            .output()
            .expect("run sb");
        assert!(output.status.success(), "{sub}: {}", stderr(&output));
        let path = world.root().join(file).display().to_string();
        assert_eq!(
            world.argv(),
            [
                world.fake(),
                "--wait".into(),
                "-c x".into(),
                "a b".into(),
                path
            ],
            "{sub}"
        );
    }
}

/// `VISUAL` comes before `EDITOR`; a blank one is skipped.
#[test]
fn visual_comes_before_editor_and_blank_is_unset() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.stub("other-editor");
    let other = world.bin().join("other-editor").display().to_string();
    let first = |visual: &str, editor: &str| {
        let output = world
            .sb("settings")
            .env("VISUAL", visual)
            .env("EDITOR", editor)
            .output()
            .expect("run sb");
        assert!(output.status.success(), "{}", stderr(&output));
        world.argv()[0].clone()
    };
    assert_eq!(first(&world.fake(), &other), world.fake());
    assert_eq!(first("", &other), other);
    assert_eq!(first("  ", &other), other);
}

/// With neither set, the first of sensible-editor, editor, nano and vi
/// that starts; none found is sb-go's error.
#[test]
fn the_fallback_list_is_tried_in_order() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let first = |world: &World| {
        let output = world.sb("settings").output().expect("run sb");
        assert!(output.status.success(), "{}", stderr(&output));
        let program = world.argv()[0].clone();
        Path::new(&program)
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned()
    };
    world.stub("vi");
    assert_eq!(first(&world), "vi");
    world.stub("nano");
    assert_eq!(first(&world), "nano");
    world.stub("editor");
    assert_eq!(first(&world), "editor");
    // A name on `PATH` that isn't executable is passed over, as sb-go's
    // `exec.LookPath` passes it over.
    std::fs::write(world.bin().join("sensible-editor"), "").expect("not executable");
    assert_eq!(first(&world), "editor");
    world.stub("sensible-editor");
    assert_eq!(first(&world), "sensible-editor");

    // One that is found and executable but doesn't start is the error,
    // as sb-go's run of what `exec.LookPath` found would be.
    let broken = World::new();
    broken.stub("vi");
    let unrunnable = broken.bin().join("editor");
    std::fs::write(&unrunnable, "").expect("empty");
    std::fs::set_permissions(&unrunnable, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let output = broken.sb("settings").output().expect("run sb");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("find editor \"editor\": "),
        "{}",
        stderr(&output)
    );
    assert!(!broken.ran());
    let path = world.root().join("settings.yml").display().to_string();
    assert_eq!(world.argv()[1..], [path]);

    let none = World::new();
    let output = none.sb("settings").output().expect("run sb");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no editor found; set VISUAL or EDITOR to an executable editor"),
        "{}",
        stderr(&output)
    );
}

/// A named editor that doesn't start is sb-go's `find editor` error.
#[test]
fn a_named_editor_that_does_not_start_fails() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let missing = world.bin().join("no-such-editor").display().to_string();
    let output = world
        .sb("settings")
        .env("EDITOR", &missing)
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(&format!("find editor {missing:?}: ")),
        "{}",
        stderr(&output)
    );
}

/// A variable that doesn't split is refused, and its value isn't echoed.
#[test]
fn an_invalid_variable_is_refused_without_its_value() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb("settings")
        .env("VISUAL", format!("{} \"unclosed-not-shown", world.fake()))
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains("invalid VISUAL environment variable"),
        "{text}"
    );
    assert!(!text.contains("unclosed-not-shown"), "{text}");
    assert!(!world.ran());
}

/// The target checks: a missing file, a dangling symlink, a directory and
/// a FIFO are refused before any editor runs; a symlink to a regular file
/// is opened by its own path.
#[test]
fn the_target_checks_refuse_what_is_not_a_regular_file() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let settings = world.root().join("settings.yml");
    let refused = |expected: &str| {
        let output = world
            .sb("settings")
            .env("VISUAL", world.fake())
            .output()
            .expect("run sb");
        assert_eq!(output.status.code(), Some(1));
        let text = stderr(&output);
        assert!(
            text.contains(&format!("{expected}: {}", settings.display())),
            "{text}"
        );
        assert!(!world.ran());
    };
    std::fs::remove_file(&settings).expect("remove");
    refused("configuration file does not yet exist");
    std::os::unix::fs::symlink(world.root().join("gone.yml"), &settings).expect("symlink");
    refused("configuration file does not yet exist");
    std::fs::remove_file(&settings).expect("remove");
    std::fs::create_dir(&settings).expect("dir");
    refused("configuration path is not a regular file");
    std::fs::remove_dir(&settings).expect("rmdir");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &settings,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .expect("fifo");
    refused("configuration path is not a regular file");
    std::fs::remove_file(&settings).expect("remove");

    let real = world.root().join("real.yml");
    std::fs::write(&real, "a: 1\n").expect("real");
    std::os::unix::fs::symlink(&real, &settings).expect("symlink");
    let output = world
        .sb("settings")
        .env("VISUAL", world.fake())
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(world.argv()[1..], [settings.display().to_string()]);
}

/// A control character in the path is escaped in the error.
#[test]
fn a_path_is_escaped_in_the_error() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let root = world.dir.path().join("salt\x1bbox");
    let output = world
        .sb("settings")
        .env("SB_SALTBOX_DIR", &root)
        .env("VISUAL", world.fake())
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1));
    let text = stderr(&output);
    assert!(
        text.contains("configuration file does not yet exist"),
        "{text}"
    );
    assert!(!text.contains('\x1b'), "{text}");
}

/// An editor that exits non-zero is `error opening editor`, exit 1.
#[test]
fn an_editor_failing_is_an_error() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb("settings")
        .env("VISUAL", world.fake())
        .env("FAKE_EXIT", "3")
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("error opening editor: exited with code 3"),
        "{}",
        stderr(&output)
    );
}

/// The run log holds the file, the editor's program, its ending and
/// whether the bytes changed, never what the file holds.
#[test]
fn the_run_log_records_the_edit_but_not_the_file() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .sb("accounts")
        .env("VISUAL", world.fake())
        .env("FAKE_APPEND", "password: hunter2-not-real\n")
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let log = world.log();
    assert!(log.contains("editor ended"), "{log}");
    assert!(log.contains("accounts.yml"), "{log}");
    assert!(log.contains("fake-editor"), "{log}");
    assert!(log.contains("exit 0"), "{log}");
    assert!(
        log.contains("changed=\"yes\"") || log.contains("changed=yes"),
        "{log}"
    );
    assert!(!log.contains("hunter2-not-real"), "{log}");

    std::fs::remove_file(world.logs().join("sb.log")).expect("old log");
    let output = world
        .sb("accounts")
        .env("VISUAL", world.fake())
        .output()
        .expect("run sb");
    assert!(output.status.success(), "{}", stderr(&output));
    let log = world.log();
    assert!(
        log.contains("changed=\"no\"") || log.contains("changed=no"),
        "{log}"
    );
}

/// SIGTERM to sb while the editor runs: the editor gets SIGTERM and sb
/// ends by the signal (143 to a shell).
#[test]
fn a_sigterm_reaches_the_editor_and_ends_sb() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut child = world
        .sb("settings")
        .env("VISUAL", world.fake())
        .env("FAKE_WAIT", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run sb");
    let pid_file = PathBuf::from(format!("{}.pid", world.argv_file().display()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pid_file.exists(), "the editor started");
    let sb = Pid::from_raw(i32::try_from(child.id()).expect("pid")).expect("pid");
    kill_process(sb, Signal::TERM).expect("kill");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            // SIGKILL to sb doesn't reach the editor, which would wait on.
            if let Some(editor) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<i32>().ok())
                .and_then(Pid::from_raw)
            {
                let _ = kill_process(editor, Signal::KILL);
            }
            panic!("sb didn't end after SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.signal(), Some(15), "{status:?}");
    assert!(
        PathBuf::from(format!("{}.term", world.argv_file().display())).exists(),
        "the editor got SIGTERM"
    );
}

/// Bare `sb edit` shows its help until the picker comes (M53), and the
/// help is sb-go's without the wrapper sentence (tui spec, difference 27).
#[test]
fn bare_edit_shows_the_help() {
    let output = common::sb().arg("edit").output().expect("run sb");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for (sub, _) in FILES {
        assert!(text.contains(&format!("    {sub} ")), "{text}");
    }
    let output = common::sb()
        .args(["edit", "--help"])
        .output()
        .expect("run sb");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("VISUAL takes precedence over EDITOR"),
        "{text}"
    );
    assert!(!text.contains("Editor wrappers"), "{text}");
}
