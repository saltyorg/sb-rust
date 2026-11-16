//! `sb edit <file>` and the picker of bare `sb edit` through the binary
//! with a fake editor (tui spec, "Editors", Testing strategy; module build
//! M49, M50 and M53): a shell script that records its name and arguments,
//! can save a prepared text or append bytes to the file, can record what
//! waits on its terminal, exits with a chosen code, or waits for a
//! signal. Every test uses its own Saltbox root (sb-config's valid
//! fixture set and its schemas), log directory and `PATH`; none opens a
//! real editor, and no credential check reaches a real service.

mod common;
mod root_gate;

use std::io::{Read as _, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, SlavePty, native_pty_system};
use rustix::process::{Pid, Signal, kill_process};
use sb_config::testing::{Fake, saltbox_root};

/// The fake editor: `$0` and each argument on its own line to
/// `$FAKE_ARGV`, its run counted in `$FAKE_ARGV.runs`; on run `n`,
/// `$FAKE_SAVES.n` (when there is one) saved over its last argument (the
/// file), then `$FAKE_APPEND` appended to it, then exit `$FAKE_EXIT`.
/// With `FAKE_WAIT` set it writes its pid to `$FAKE_ARGV.pid` and waits;
/// a SIGTERM writes `$FAKE_ARGV.term` and ends it. With `FAKE_STDIN` set it
/// first prints [`STARTED`], then saves what its terminal holds unread
/// (waiting up to 0.2 s) to `$FAKE_ARGV.stdin`.
const FAKE: &str = r#"#!/bin/sh
printf '%s\n' "$0" "$@" > "$FAKE_ARGV"
if [ -n "$FAKE_STDIN" ]; then
  echo "fake editor started"
  /bin/stty -icanon min 0 time 2
  /bin/dd bs=4096 count=1 of="$FAKE_ARGV.stdin" 2>/dev/null
  /bin/stty icanon
fi
for last; do :; done
runs=$(( $(/bin/cat "$FAKE_ARGV.runs" 2>/dev/null || echo 0) + 1 ))
echo "$runs" > "$FAKE_ARGV.runs"
if [ -n "$FAKE_SAVES" ] && [ -f "$FAKE_SAVES.$runs" ]; then /bin/cat "$FAKE_SAVES.$runs" > "$last"; fi
if [ -n "$FAKE_APPEND" ]; then printf '%s' "$FAKE_APPEND" >> "$last"; fi
if [ -n "$FAKE_WAIT" ]; then
  trap 'echo term > "$FAKE_ARGV.term"; exit 0' TERM
  echo $$ > "$FAKE_ARGV.pid"
  while :; do /bin/sleep 0.05; done
fi
exit "${FAKE_EXIT:-0}"
"#;

/// What the fake prints first with `FAKE_STDIN` set.
const STARTED: &str = "fake editor started";

/// The six files and their subcommands, as sb-go's layout has them.
const FILES: [(&str, &str); 6] = [
    ("accounts", "accounts.yml"),
    ("settings", "settings.yml"),
    ("adv_settings", "adv_settings.yml"),
    ("backup_config", "backup_config.yml"),
    ("hetzner_vlan", "hetzner_vlan.yml"),
    ("inventory", "inventories/host_vars/localhost.yml"),
];

/// A setting sb-go's schema doesn't know: one issue in any schema-backed
/// file, `<file>:<line>:1: not_a_setting: unknown field`.
const UNKNOWN: &str = "not_a_setting: 1\n";

/// A test's world: a Saltbox root holding the six files, valid (sb-config's
/// fixture set, its schemas, and an inventory), a `bin` directory for
/// `PATH`, the fake editor in it, and a log directory.
struct World {
    dir: tempfile::TempDir,
    root: tempfile::TempDir,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        let root = saltbox_root("valid").expect("root");
        std::fs::create_dir_all(root.path().join("inventories/host_vars")).expect("inventory");
        std::fs::write(
            root.path().join("inventories/host_vars/localhost.yml"),
            "a: 1\n",
        )
        .expect("inventory");
        std::fs::create_dir(dir.path().join("bin")).expect("bin");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        let world = Self { dir, root };
        world.stub("fake-editor");
        world
    }

    fn root(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    /// `file`'s text under the root.
    fn text(&self, file: &str) -> String {
        std::fs::read_to_string(self.root().join(file)).expect("read")
    }

    /// The fake's saves: run `n` saves `texts[n - 1]` over the file.
    fn saves(&self, texts: &[&str]) -> PathBuf {
        let saves = self.dir.path().join("save");
        for (n, text) in texts.iter().enumerate() {
            std::fs::write(format!("{}.{}", saves.display(), n + 1), text).expect("save");
        }
        saves
    }

    /// How many times the fake ran.
    fn runs(&self) -> usize {
        std::fs::read_to_string(format!("{}.runs", self.argv_file().display()))
            .map_or(0, |text| text.trim().parse().expect("a count"))
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
        self.edit(&[sub])
    }

    /// `sb edit` with `args`, as [`World::sb`].
    fn edit(&self, args: &[&str]) -> Command {
        let mut cmd = common::sb();
        cmd.arg("edit").args(args);
        self.apply(&mut cmd);
        cmd
    }

    /// The world's environment on `cmd`, after `common::apply_sb_env`'s.
    fn apply(&self, cmd: &mut Command) {
        cmd.env("SB_SALTBOX_DIR", self.root())
            .env("SB_LOG_DIR", self.logs())
            .env("PATH", self.bin())
            .env("FAKE_ARGV", self.argv_file())
            .env_remove("VISUAL")
            .env_remove("EDITOR");
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
    let valid = world.text("settings.yml");
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
    std::fs::write(&real, valid).expect("real");
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
    // The editor reported its own failure: nothing is validated.
    let text = stderr(&output);
    assert!(!text.contains("settings.yml"), "{text}");
    assert!(
        !world.log().contains("config file checked"),
        "{}",
        world.log()
    );
}

/// The run log holds the file, the editor's program, its ending,
/// whether the bytes changed and the validation's issue count, never what
/// the file holds.
#[test]
fn the_run_log_records_the_edit_but_not_the_file() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let accounts = world.text("accounts.yml");
    let secret = accounts.replace("pass: correct-horse-battery", "pass: hunter2-not-real");
    assert_ne!(secret, accounts, "the fixture changed");
    let output = world
        .sb("accounts")
        .env("VISUAL", world.fake())
        .env("FAKE_SAVES", world.saves(&[&secret]))
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
    assert!(log.contains("edited file validated"), "{log}");
    assert!(logged(&log, "issues", "0"), "{log}");
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

/// A file with issues: the count is logged, and a secret saved into the
/// file shows neither there nor on screen, where its issue names the key.
#[test]
fn the_run_log_counts_the_issues_without_a_secret() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let accounts = world.text("accounts.yml");
    let secret = accounts.replace(
        "pass: correct-horse-battery",
        "pass: !!int hunter2-not-real",
    );
    assert_ne!(secret, accounts, "the fixture changed");
    let output = world
        .sb("accounts")
        .arg("--no-edit-again")
        .env("VISUAL", world.fake())
        .env("FAKE_SAVES", world.saves(&[&secret]))
        .output()
        .expect("run sb");
    let text = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("user.pass: cannot decode"), "{text}");
    assert!(!text.contains("hunter2-not-real"), "{text}");
    let log = world.log();
    assert!(logged(&log, "issues", "1"), "{log}");
    assert!(!log.contains("hunter2-not-real"), "{log}");
}

/// Whether `log` has the field `key` with `value`, quoted or not.
fn logged(log: &str, key: &str, value: &str) -> bool {
    log.contains(&format!("{key}={value}")) || log.contains(&format!("{key}=\"{value}\""))
}

/// A clean edit: the file's step under `sb edit`, done, and exit 0 with
/// no question asked.
#[test]
fn a_clean_edit_is_validated() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    for (sub, shown) in [
        ("settings", "settings.yml"),
        ("inventory", "localhost.yml (no duplicates)"),
    ] {
        let output = world
            .sb(sub)
            .env("VISUAL", world.fake())
            .output()
            .expect("run sb");
        let text = stderr(&output);
        assert!(output.status.success(), "{text}");
        assert!(text.starts_with("sb edit\n"), "{text}");
        assert!(text.contains(&format!("✓ Validated {shown}")), "{text}");
        assert!(!text.contains("again?"), "{text}");
    }
}

/// An edit with issues shows them as `sb validate-config` shows them for
/// that file (the same issue lines), and `--no-edit-again`, before or
/// after the subcommand, answers no: exit 1, the editor run once, and
/// the file left as the editor saved it.
#[test]
fn issues_show_as_validate_config_shows_them() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let broken = format!("{}{UNKNOWN}nor_this: 2\n", world.text("settings.yml"));
    let saves = world.saves(&[&broken]);
    let output = world
        .sb("settings")
        .arg("--no-edit-again")
        .env("VISUAL", world.fake())
        .env("FAKE_SAVES", &saves)
        .output()
        .expect("run sb");
    let edit = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{edit}");
    assert!(edit.contains("✗ Validation of settings.yml"), "{edit}");
    assert!(!edit.contains("again?"), "{edit}");
    assert_eq!(world.runs(), 1);
    assert_eq!(world.text("settings.yml"), broken);

    let fake = Fake::start().expect("fake");
    let output = common::sb()
        .arg("validate-config")
        .env("SB_SALTBOX_DIR", world.root())
        .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
        .env(
            "SB_DOCKERHUB_URL",
            format!("{}/v2/users/login/", fake.url()),
        )
        .output()
        .expect("run sb");
    let validate = stderr(&output);
    let issues = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|line| line.starts_with("settings.yml:"))
            .map(str::to_string)
            .collect()
    };
    assert_eq!(issues(&edit).len(), 2, "{edit}");
    assert_eq!(issues(&edit), issues(&validate), "{edit}\n{validate}");

    // The flag before the subcommand: the same answer.
    let output = world
        .edit(&["--no-edit-again", "settings"])
        .env("VISUAL", world.fake())
        .env("FAKE_SAVES", &saves)
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(world.runs(), 2);
}

/// With issues, no terminal and no flag, the question can't be asked: its
/// error names `--no-edit-again`, exit 1, never a hang.
#[test]
fn without_a_terminal_the_error_names_the_flag() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    // A session of its own (util-linux's `setsid`, waiting for it): no
    // controlling terminal, so no `/dev/tty` either.
    let mut cmd = Command::new("/usr/bin/setsid");
    cmd.arg("--wait")
        .arg(common::sb().get_program())
        .args(["edit", "settings"])
        .stdin(std::process::Stdio::null());
    common::apply_sb_env(&mut cmd);
    world.apply(&mut cmd);
    let output = cmd
        .env("VISUAL", world.fake())
        .env("FAKE_APPEND", UNKNOWN)
        .output()
        .expect("run sb");
    let text = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("✗ Validation of settings.yml"), "{text}");
    assert!(
        text.contains("can't ask \"Edit settings.yml again?\""),
        "{text}"
    );
    assert!(text.contains("answer it with --no-edit-again"), "{text}");
    assert_eq!(world.runs(), 1);
}

/// accounts.yml with credentials: no credential check is sent (a fake
/// server set as both services sees nothing), and the step says so.
#[test]
fn accounts_skips_the_credential_checks() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let fake = Fake::start().expect("fake");
    let output = world
        .sb("accounts")
        .env("VISUAL", world.fake())
        .env("CLOUDFLARE_BASE_URL", format!("{}/client/v4/", fake.url()))
        .env(
            "SB_DOCKERHUB_URL",
            format!("{}/v2/users/login/", fake.url()),
        )
        .output()
        .expect("run sb");
    let text = stderr(&output);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(text.contains("✓ Validated accounts.yml"), "{text}");
    assert!(text.contains(sb_config::run::CHECKS_SKIPPED), "{text}");
    assert!(fake.seen().is_empty(), "{:?}", fake.seen());
}

/// A save sb can't read (not UTF-8) is the file's issue, as
/// validate-config reports it: another edit can fix it, so the question
/// is asked (here without a terminal, so its error names the flag).
#[test]
fn a_save_that_cannot_be_read_is_offered_again() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let saves = world.saves(&[]);
    let mut latin1 = world.text("settings.yml").into_bytes();
    latin1.extend_from_slice(b"# caf\xe9\n");
    std::fs::write(format!("{}.1", saves.display()), latin1).expect("save");
    let mut cmd = Command::new("/usr/bin/setsid");
    cmd.arg("--wait")
        .arg(common::sb().get_program())
        .args(["edit", "settings"])
        .stdin(std::process::Stdio::null());
    common::apply_sb_env(&mut cmd);
    world.apply(&mut cmd);
    let output = cmd
        .env("VISUAL", world.fake())
        .env("FAKE_SAVES", &saves)
        .output()
        .expect("run sb");
    let text = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("isn't UTF-8"), "{text}");
    assert!(
        text.contains("can't ask \"Edit settings.yml again?\""),
        "{text}"
    );
    assert!(logged(&world.log(), "issues", "1"), "{}", world.log());
}

/// A file that can't be checked (its schema gone) fails its step with
/// validate-config's words, exit 1, and offers no edit: editing the file
/// can't fix it.
#[test]
fn a_file_that_cannot_be_checked_is_not_offered_again() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    std::fs::remove_file(world.root().join("schema/settings.schema.yml")).expect("schema");
    let output = world
        .sb("settings")
        .env("VISUAL", world.fake())
        .output()
        .expect("run sb");
    let text = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("schema file not found: "), "{text}");
    assert!(!text.contains("again?"), "{text}");
    assert!(
        !world.log().contains("edited file validated"),
        "{}",
        world.log()
    );
}

/// How long a PTY test waits for the screen or for sb to exit.
const WAIT: Duration = Duration::from_secs(20);

/// `sb edit` on a PTY of 100×30 (portable-pty), in the world's
/// environment, its screen parsed through vt100 as it arrives. Why not
/// `logs_pty::Pty`, the logs tests' same harness: that module also holds
/// their world and helpers, which this crate would leave unused
/// (dead-code warnings).
struct Pty {
    child: Box<dyn Child + Send + Sync>,
    // Both ends kept open, as a shell keeps its terminal: the termios
    // check reads the terminal after sb is gone.
    _master: Box<dyn MasterPty + Send>,
    _slave: Box<dyn SlavePty + Send>,
    tty: PathBuf,
    writer: Box<dyn Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    parser: vt100::Parser,
}

impl Pty {
    /// `sb edit <args>` in `world`, `extra` set after its environment.
    fn spawn(world: &World, args: &[&str], extra: &[(&str, &std::ffi::OsStr)]) -> Self {
        Self::spawn_with(world, args, extra, None)
    }

    /// [`Pty::spawn`] with sb started by `/bin/sh` as `exec sb <args>
    /// <redirect>`, so one of its streams is something other than the
    /// PTY.
    fn spawn_with(
        world: &World,
        args: &[&str],
        extra: &[(&str, &std::ffi::OsStr)],
        redirect: Option<&str>,
    ) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        // The environment as `World::edit` gives it, read back from a
        // `Command` built that way.
        let mut base = world.edit(args);
        for (key, value) in extra {
            base.env(key, value);
        }
        let mut cmd = match redirect {
            None => CommandBuilder::new(base.get_program()),
            Some(redirect) => {
                let mut sh = CommandBuilder::new("/bin/sh");
                sh.args(["-c", &format!("exec \"$0\" \"$@\" {redirect}")]);
                sh.arg(base.get_program());
                sh
            }
        };
        cmd.args(base.get_args());
        cmd.env("TERM", "xterm-256color");
        cmd.env("NO_COLOR", "1");
        for (key, value) in base.get_envs() {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            }
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn");
        let tty = pair.master.tty_name().expect("tty name");
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer = pair.master.take_writer().expect("writer");
        let (tx, output) = mpsc::channel();
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
            _slave: pair.slave,
            tty,
            writer,
            output,
            parser: vt100::Parser::new(30, 100, 0),
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write");
        self.writer.flush().expect("flush");
    }

    fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    /// Wait until the screen shows `text`; false at the timeout.
    fn showing(&mut self, text: &str) -> bool {
        let deadline = Instant::now() + WAIT;
        loop {
            if self.contents().contains(text) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.output.recv_timeout(left) {
                Ok(bytes) => self.parser.process(&bytes),
                Err(_) => return self.contents().contains(text),
            }
        }
    }

    /// Wait until the screen is `done`; false at the timeout.
    fn wait_screen(&mut self, done: impl Fn(&vt100::Screen) -> bool) -> bool {
        let deadline = Instant::now() + WAIT;
        while !done(self.parser.screen()) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.output.recv_timeout(left) {
                Ok(bytes) => self.parser.process(&bytes),
                Err(_) => return done(self.parser.screen()),
            }
        }
        true
    }

    /// Take the output already here, without waiting.
    fn take_now(&mut self) {
        while let Ok(bytes) = self.output.try_recv() {
            self.parser.process(&bytes);
        }
    }

    /// Wait for sb to exit, reading its output meanwhile.
    fn exit(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                while let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(200)) {
                    self.parser.process(&bytes);
                }
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "sb didn't exit:\n{}",
                self.contents()
            );
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
                self.parser.process(&bytes);
            }
        }
    }

    /// The terminal is restored: no alternate screen or mouse capture
    /// left, the cursor shown, and the PTY back in canonical mode with
    /// echo.
    fn assert_restored(&self) {
        let screen = self.parser.screen();
        assert!(!screen.alternate_screen(), "{}", self.contents());
        assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
        assert!(!screen.hide_cursor(), "{}", self.contents());
        let tty = rustix::fs::open(
            &self.tty,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOCTTY,
            rustix::fs::Mode::empty(),
        )
        .expect("tty");
        let modes = rustix::termios::tcgetattr(&tty)
            .expect("termios")
            .local_modes;
        assert!(
            modes.contains(rustix::termios::LocalModes::ICANON | rustix::termios::LocalModes::ECHO),
            "{modes:?}"
        );
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// sb edit settings on a PTY whose fake saves `saves` run by run, up to
/// the question.
fn asked(world: &World, saves: &[&str]) -> Pty {
    let saves = world.saves(saves);
    let fake = world.fake();
    let mut pty = Pty::spawn(
        world,
        &["settings"],
        &[("VISUAL", fake.as_ref()), ("FAKE_SAVES", saves.as_os_str())],
    );
    assert!(
        pty.showing("Edit settings.yml again? [y/N]"),
        "{}",
        pty.contents()
    );
    pty
}

/// "Edit again?" answered yes runs the editor on the file again, which
/// fixes it this time: validated, exit 0.
#[test]
fn yes_edits_the_file_again() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let valid = world.text("settings.yml");
    let broken = format!("{valid}{UNKNOWN}");
    let mut pty = asked(&world, &[&broken, &valid]);
    assert!(
        pty.contents().contains("✗ Validation of settings.yml"),
        "{}",
        pty.contents()
    );
    pty.send(b"y\r");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 0, "{}", pty.contents());
    assert_eq!(world.runs(), 2);
    assert!(
        pty.contents().contains("✓ Validated settings.yml"),
        "{}",
        pty.contents()
    );
    assert_eq!(world.text("settings.yml"), valid);
    pty.assert_restored();
}

/// Answered no (Enter alone, the default): exit 1, the file as the
/// editor saved it.
#[test]
fn no_ends_with_the_issues() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let broken = format!("{}{UNKNOWN}", world.text("settings.yml"));
    let mut pty = asked(&world, &[&broken]);
    pty.send(b"\r");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 1, "{}", pty.contents());
    assert_eq!(world.runs(), 1);
    assert_eq!(world.text("settings.yml"), broken);
    pty.assert_restored();
}

/// Ctrl+C at the question: sb ends by SIGINT (130 to a shell), the
/// terminal restored, the editor not run again.
#[test]
fn ctrl_c_at_the_question_interrupts() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let broken = format!("{}{UNKNOWN}", world.text("settings.yml"));
    let mut pty = asked(&world, &[&broken]);
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(
        status.signal(),
        Some("Interrupt"),
        "{status:?}\n{}",
        pty.contents()
    );
    assert!(pty.contents().contains("interrupted"), "{}", pty.contents());
    assert_eq!(world.runs(), 1);
    pty.assert_restored();
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

/// `sb edit --help` is sb-go's help without the wrapper sentence (tui
/// spec, difference 27) or the GUI-client one (difference 34), and lists
/// the six subcommands.
#[test]
fn the_help_is_sb_go_s_with_the_differences() {
    let output = common::sb()
        .args(["edit", "--help"])
        .output()
        .expect("run sb");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for (sub, _) in FILES {
        assert!(text.contains(&format!("    {sub} ")), "{text}");
    }
    assert!(
        text.contains("VISUAL takes precedence over EDITOR"),
        "{text}"
    );
    assert!(!text.contains("Editor wrappers"), "{text}");
    assert!(!text.contains("GUI"), "{text}");
    // The validation's line, and the flag on `edit` and each subcommand.
    assert!(text.contains("sb offers to edit it again"), "{text}");
    for args in [&["edit", "--help"][..], &["edit", "settings", "--help"]] {
        let output = common::sb().args(args).output().expect("run sb");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("--no-edit-again"), "{args:?}: {text}");
    }
}

/// Bare `sb edit` without a terminal on stdin and stdout refuses before
/// drawing, naming `sb edit <file>`; no editor runs.
#[test]
fn the_picker_refuses_without_a_terminal() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world
        .edit(&[])
        .env("VISUAL", world.fake())
        .output()
        .expect("run sb");
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("sb edit requires an interactive terminal; use sb edit <file>"),
        "{}",
        stderr(&output)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(!world.ran());
}

/// The picker needs both stdin and stdout to be a terminal: either one
/// redirected away from the PTY refuses, before anything is drawn.
#[test]
fn the_picker_refuses_with_only_one_stream_on_a_terminal() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let out = world.dir.path().join("out");
    for redirect in [format!("> '{}'", out.display()), "< /dev/null".to_owned()] {
        let fake = world.fake();
        let mut pty = Pty::spawn_with(&world, &[], &[("VISUAL", fake.as_ref())], Some(&redirect));
        let status = pty.exit();
        assert_eq!(status.exit_code(), 1, "{redirect}: {}", pty.contents());
        let shown = format!(
            "{}{}",
            pty.contents(),
            std::fs::read_to_string(&out).unwrap_or_default()
        );
        assert!(
            shown.contains("sb edit requires an interactive terminal; use sb edit <file>"),
            "{redirect}: {shown}"
        );
        assert!(!shown.contains("Select a Saltbox"), "{redirect}: {shown}");
        assert!(!world.ran(), "{redirect}");
    }
}

// --- the picker on a PTY (M53) ---

/// Bare `sb edit` on a PTY, the fake as `VISUAL` recording its terminal,
/// up to the picker.
fn picker(world: &World) -> Pty {
    let fake = world.fake();
    let mut pty = Pty::spawn(
        world,
        &[],
        &[("VISUAL", fake.as_ref()), ("FAKE_STDIN", "1".as_ref())],
    );
    assert!(
        pty.showing("Select a Saltbox configuration file to edit:"),
        "{}",
        pty.contents()
    );
    assert!(pty.parser.screen().alternate_screen());
    assert_ne!(
        pty.parser.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );
    pty
}

/// Once the fake has started: the screen as it found it was restored (no
/// alternate screen, no mouse capture), and nothing waited on its terminal.
fn assert_editor_got_a_clean_terminal(world: &World, pty: &mut Pty) {
    assert!(pty.showing(STARTED), "{}", pty.contents());
    let screen = pty.parser.screen();
    assert!(!screen.alternate_screen(), "{}", pty.contents());
    assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
    let stdin_file = PathBuf::from(format!("{}.stdin", world.argv_file().display()));
    let deadline = Instant::now() + WAIT;
    while !stdin_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // dd writes the file at its open, then what it read.
    let status = pty.exit();
    let unread = std::fs::read(&stdin_file).expect("the fake read its terminal");
    assert_eq!(String::from_utf8_lossy(&unread), "");
    assert_eq!(status.exit_code(), 0, "{}", pty.contents());
}

/// Down twice and Enter open the third file, Advanced Settings, in the
/// editor once the picker has gone; it is validated as `sb edit
/// adv_settings` validates it, and sb exits 0.
#[test]
fn enter_opens_the_chosen_file_in_the_editor() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = picker(&world);
    pty.send(b"\x1b[B");
    pty.send(b"j");
    pty.send(b"\r");
    assert_editor_got_a_clean_terminal(&world, &mut pty);
    let path = world.root().join("adv_settings.yml").display().to_string();
    assert_eq!(world.argv(), [world.fake(), path]);
    assert!(
        pty.contents().contains("Validated adv_settings.yml"),
        "{}",
        pty.contents()
    );
    pty.assert_restored();
    assert!(
        logged(&world.log(), "chosen", "adv_settings.yml"),
        "{}",
        world.log()
    );
}

/// An SGR mouse report: a left press at `column`, `row` (1-based).
fn left_click(column: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").into_bytes()
}

/// A double click on a row opens its file. Mouse reports the terminal
/// sends once the picker has turned mouse capture off, while it leaves,
/// never reach the editor: the picker reads them off before it starts.
#[test]
fn a_double_click_opens_the_file_and_no_mouse_bytes_reach_the_editor() {
    if !root_gate::requested() {
        return;
    }
    // A run where the harness was too slow to send the reports while the
    // picker was still leaving proves nothing either way; it runs again
    // (as sb-tui's own drain test).
    for _ in 0..3 {
        let world = World::new();
        let mut pty = picker(&world);
        // The list's first row is the screen's third; Hetzner VLAN
        // Settings is the fifth file, on the seventh row.
        let click = left_click(5, 7);
        pty.send(&click);
        pty.send(&click);
        assert!(
            pty.wait_screen(|s| s.mouse_protocol_mode() == vt100::MouseProtocolMode::None),
            "{}",
            pty.contents()
        );
        for after in [0, 10, 20] {
            std::thread::sleep(Duration::from_millis(after));
            pty.send(b"\x1b[<35;10;5M");
        }
        pty.take_now();
        if !pty.parser.screen().alternate_screen() {
            // Already left: the reports may have come after the drain.
            let _ = pty.exit();
            continue;
        }
        assert_editor_got_a_clean_terminal(&world, &mut pty);
        let path = world.root().join("hetzner_vlan.yml").display().to_string();
        assert_eq!(world.argv(), [world.fake(), path]);
        assert!(!pty.contents().contains("<35;10;5M"), "{}", pty.contents());
        pty.assert_restored();
        return;
    }
    panic!("the harness never sent the reports while the picker left");
}

/// SIGTERM to sb in the picker: no editor, the terminal restored, and sb
/// ends by the signal (143 to a shell).
#[test]
fn a_sigterm_in_the_picker_restores_the_terminal() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = picker(&world);
    let pid = pty
        .child
        .process_id()
        .and_then(|pid| Pid::from_raw(i32::try_from(pid).ok()?))
        .expect("sb's pid");
    kill_process(pid, Signal::TERM).expect("kill");
    let status = pty.exit();
    assert_eq!(
        status.signal(),
        Some("Terminated"),
        "{status:?}\n{}",
        pty.contents()
    );
    assert!(!world.ran());
    pty.assert_restored();
}

/// `q` and Esc leave the picker: exit 0, no editor, the terminal restored.
#[test]
fn q_and_esc_leave_without_an_editor() {
    if !root_gate::requested() {
        return;
    }
    for key in [&b"q"[..], b"\x1b"] {
        let world = World::new();
        let mut pty = picker(&world);
        pty.send(key);
        let status = pty.exit();
        assert_eq!(status.exit_code(), 0, "{key:?}: {}", pty.contents());
        assert!(!world.ran(), "{key:?}");
        pty.assert_restored();
    }
}

/// Ctrl-C in the picker: exit 130, quietly, no editor, the terminal
/// restored.
#[test]
fn ctrl_c_in_the_picker_exits_130() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = picker(&world);
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    assert!(!world.ran());
    pty.assert_restored();
}
