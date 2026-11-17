//! `sb announcements` end to end (ops spec, Testing Strategy,
//! "`announcements`"): Saltbox's and Sandbox's own files in a temporary
//! directory, which is also the Saltbox root (Sandbox's checkout under the
//! appdata path its inventory names there), and the inline output on
//! stdout. The command takes no flags of its own (the maintainer,
//! 2026-11-17) and calls nothing new. Migrations are never listed, asked
//! about or run here (a stand-in `ansible-playbook` that would leave a
//! mark). Root runs (`SB_TEST_ROOT=1`): the command needs root; each piped
//! run has no controlling terminal (`setsid -w`). On a PTY ([`Pty`], as the
//! log screens' harness), the viewer opens and closes.

mod common;
mod root_gate;

use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

/// Saltbox's file: one old entry, two out of date order (one asking for a
/// migration, one with a code block), and one gated by a missing file
/// (`MISSING`, a path in the test's directory: [`Files::saltbox_file`]).
const SALTBOX: &str = "---\nannouncements:\n  - date: \"2026-01-10\"\n    title: \"Old news\"\n    \
                       message: |\n      Already seen.\n  - date: \"2026-03-02\"\n    \
                       title: \"Settings moved\"\n    migration:\n      required: true\n      \
                       tag: settings_move\n    message: |\n      ## Settings moved\n\n      \
                       Run the **migration** when asked.\n  - date: \"2026-02-14\"\n    \
                       title: \"New defaults\"\n    message: |\n      ### What changed\n\n      \
                       ```yaml\n      network: saltbox\n      ```\n  - date: \"2026-02-20\"\n    \
                       title: \"Gated\"\n    required_file: \"MISSING\"\n    \
                       message: Not shown.\n";

/// Saltbox's file with its one old entry.
const SALTBOX_ONE: &str = "---\nannouncements:\n  - date: \"2026-01-10\"\n    title: \"Old news\"\n    \
                           message: |\n      Already seen.\n";

/// Sandbox's file: one entry.
const SANDBOX: &str = "announcements:\n  - date: \"2026-04-01\"\n    title: \"Sandbox news\"\n    \
                       message: From the sandbox.\n";

/// A file that doesn't load.
const INVALID: &str = "announcements:\n  - title: [x]\n";

/// [`SALTBOX`] inline: every entry that applies, oldest first, each
/// headed with its repository.
const SALTBOX_SHOWN: &str = "# Saltbox Announcement - January 10, 2026\n\nAlready seen.\n\n\
                             # Saltbox Announcement - February 14, 2026\n\n### What changed\n\n\
                             ```yaml\nnetwork: saltbox\n```\n\n\
                             # Saltbox Announcement - March 2, 2026\n\n## Settings moved\n\n\
                             Run the migration when asked.\n";

const SALTBOX_ONE_SHOWN: &str = "# Saltbox Announcement - January 10, 2026\n\nAlready seen.\n";
const SANDBOX_SHOWN: &str = "# Sandbox Announcement - April 1, 2026\n\nFrom the sandbox.\n";

const TABLE_LINE: &str = "The following tags need to be run to handle migrations:";

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("dir");
        // The inventory puts the appdata path, and so Sandbox's checkout,
        // in the directory too: no run reads the host's.
        let inventory = dir.path().join("inventories/host_vars");
        std::fs::create_dir_all(&inventory).expect("inventory dir");
        std::fs::write(
            inventory.join("localhost.yml"),
            format!(
                "server_appdata_path: {}\n",
                dir.path().join("appdata").display()
            ),
        )
        .expect("inventory");
        // A playbook stand-in that would mark any run.
        let playbook = dir.path().join("ansible-playbook");
        std::fs::write(
            &playbook,
            format!("#!/bin/sh\ntouch '{}'\n", dir.path().join("ran").display()),
        )
        .expect("stand-in");
        std::fs::set_permissions(&playbook, std::fs::Permissions::from_mode(0o755)).expect("mode");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Saltbox's own file, at the Saltbox root; `MISSING` made a path in
    /// the directory that can't exist, so no file on the host decides the
    /// gated entry.
    fn saltbox_file(&self, text: &str) {
        let missing = self.path("missing/file");
        let text = text.replace("MISSING", &missing.display().to_string());
        std::fs::write(self.path("announcements.yml"), text).expect("saltbox file");
    }

    /// Sandbox's own file: `<appdata>/sandbox`, its checkout created.
    fn sandbox_file(&self, text: &str) {
        let checkout = self.path("appdata/sandbox");
        std::fs::create_dir_all(&checkout).expect("checkout");
        std::fs::write(checkout.join("announcements.yml"), text).expect("sandbox file");
    }

    /// `sb announcements <args>` with no controlling terminal, the
    /// stand-in as the playbook and on `PATH`, the run log in `logs` if
    /// given.
    fn run_logged(&self, args: &[&str], logs: Option<&Path>) -> Output {
        let sb = assert_cmd::cargo::cargo_bin("sb");
        let mut cmd = Command::new("setsid");
        common::apply_sb_env(&mut cmd);
        cmd.env("SB_ANSIBLE_PLAYBOOK", self.path("ansible-playbook"));
        cmd.env("SB_SALTBOX_DIR", self.dir.path());
        if let Some(logs) = logs {
            cmd.env("SB_LOG_DIR", logs);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![self.dir.path().to_path_buf()];
        dirs.extend(std::env::split_paths(&path));
        cmd.env("PATH", std::env::join_paths(dirs).expect("PATH"));
        cmd.arg("-w").arg(sb).arg("announcements").args(args);
        cmd.current_dir(self.dir.path());
        cmd.output().expect("run sb")
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_logged(args, None)
    }

    /// The environment of a PTY run: the Saltbox root, and the stand-in
    /// as the playbook and first on `PATH`, as [`Files::run`] sets them.
    fn pty_envs(&self) -> Vec<(&'static str, String)> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![self.dir.path().to_path_buf()];
        dirs.extend(std::env::split_paths(&path));
        vec![
            ("SB_SALTBOX_DIR", self.dir.path().display().to_string()),
            (
                "SB_ANSIBLE_PLAYBOOK",
                self.path("ansible-playbook").display().to_string(),
            ),
            (
                "PATH",
                std::env::join_paths(dirs)
                    .expect("PATH")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]
    }

    fn ran(&self) -> bool {
        self.path("ran").exists()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn both(out: &Output) -> String {
    format!("stdout:\n{}\nstderr:\n{}", stdout(out), stderr(out))
}

/// Every entry that applies inline, oldest first, headed with its
/// repository; an entry asking for a migration is shown, but nothing is
/// listed, asked or run: migrations are `sb update`'s. Nothing is called
/// new.
#[test]
fn it_shows_every_entry_and_no_migrations() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX);
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    let shown = stdout(&out);
    assert_eq!(shown, SALTBOX_SHOWN);
    assert!(!shown.contains('\x1b'), "piped: no colour\n{shown}");
    let steps = stderr(&out);
    // The step counts what is shown: the three that apply.
    assert!(steps.contains("› 3 announcements"), "{steps}");
    assert!(steps.contains("Read announcements"), "{steps}");
    let lower = both(&out).to_lowercase();
    assert!(
        !lower.contains(" new ") && !lower.contains("new announcement"),
        "\"new\" is sb update's word: {lower}"
    );
    for text in [TABLE_LINE, "Run all migration tags?", "Would execute"] {
        assert!(!both(&out).contains(text), "{text}: {}", both(&out));
    }
    assert!(!files.ran(), "a playbook ran");
}

/// `--migrations` went with the migrations, and `--before`, `--after` and
/// `--repo` with the comparing and the narrowing (the maintainer,
/// 2026-11-17): each, long or short, is clap's unknown-argument error,
/// exit 2, before anything runs.
#[test]
fn it_takes_no_flags_of_its_own() {
    for args in [
        &["--migrations", "run"][..],
        &["--before", "x.yml"],
        &["-b", "x.yml"],
        &["--after", "x.yml"],
        &["-a", "x.yml"],
        &["--repo", "Saltbox"],
        &["-r", "Sandbox"],
    ] {
        let out = common::sb()
            .arg("announcements")
            .args(args)
            .arg("--color=never")
            .output()
            .expect("run sb");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", both(&out));
        assert!(
            stderr(&out).contains(&format!("unexpected argument '{}'", args[0])),
            "{args:?}: {}",
            both(&out)
        );
    }
}

/// Nothing to show: no output on stdout, and the step says so.
#[test]
fn nothing_to_show_prints_nothing() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), "", "{}", both(&out));
    assert!(
        stderr(&out).contains("› No announcements"),
        "{}",
        both(&out)
    );
}

#[test]
fn colour_follows_the_choice() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX);
    let out = files.run(&["--color=always"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    let shown = stdout(&out);
    assert!(shown.contains("\x1b["), "{shown}");
    assert!(
        !shown.contains("## Settings moved"),
        "markers hidden\n{shown}"
    );
    assert!(shown.contains("Settings moved"), "{shown}");
}

/// Shown in `sb --help` (the maintainer, 2026-11-15); its own help has no
/// flag of its own: not sb-go's `--before`, `--after`, `--repo` or `-v`,
/// and no `--migrations`; it says both repositories are shown, and never
/// "new".
#[test]
fn its_help_has_no_flags_of_its_own() {
    let out = common::sb()
        .args(["--help", "--color=never"])
        .output()
        .expect("run sb");
    assert!(
        stdout(&out).contains("Display announcements from announcement files"),
        "{}",
        both(&out)
    );
    let out = common::sb()
        .args(["announcements", "--help", "--color=never"])
        .output()
        .expect("run sb");
    let help = stdout(&out);
    for flag in ["--before", "--after", "--repo", "--verbose", "--migrations"] {
        assert!(!help.contains(flag), "{flag}: {help}");
    }
    assert!(
        help.contains("Display Saltbox's and Sandbox's announcements"),
        "{help}"
    );
    assert!(!help.contains("new"), "\"new\" is sb update's word: {help}");
}

/// On a terminal, the viewer: every entry that applies, each row labelled
/// with its repository, nothing called new; `q` closes it, exit 0, the
/// terminal restored.
#[test]
fn on_a_terminal_the_viewer_lists_every_entry_and_q_closes_it() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX);
    let logs = files.path("logs");
    std::fs::create_dir(&logs).expect("logs");
    let mut envs = files.pty_envs();
    envs.push(("SB_LOG_DIR", logs.display().to_string()));
    let mut pty = Pty::spawn(&["announcements"], &envs);
    assert!(pty.showing("Settings moved"), "{}", pty.contents());
    let screen = pty.contents();
    for text in [
        "2026-03-02  Saltbox  Settings moved",
        "2026-02-14  Saltbox  New defaults",
        "2026-01-10  Saltbox  Old news",
        "╭ Announcements ─",
    ] {
        assert!(screen.contains(text), "{text}: {screen}");
    }
    for text in ["Gated", " NEW ", "● "] {
        assert!(!screen.contains(text), "{text}: {screen}");
    }
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{status:?}\n{}", pty.contents());
    pty.assert_restored();
    // The step counted what the viewer showed: the three that apply.
    assert!(
        pty.contents().contains("› 3 announcements"),
        "{}",
        pty.contents()
    );
    assert!(!files.ran(), "a playbook ran");
    let log = std::fs::read_to_string(logs.join("sb.log")).unwrap_or_default();
    assert!(log.contains("announcement viewer opened"), "{log}");
    assert!(
        !log.contains("Settings moved"),
        "no title in the log: {log}"
    );
}

/// Ctrl-C on the viewer: exit 130, quietly, the terminal restored.
#[test]
fn ctrl_c_on_the_viewer_exits_130() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX);
    let mut pty = Pty::spawn(&["announcements"], &files.pty_envs());
    assert!(pty.showing("╭ Announcements ─"), "{}", pty.contents());
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    pty.assert_restored();
    assert!(!pty.contents().contains("ERROR"), "{}", pty.contents());
}

/// On a terminal with nothing that applies, no viewer opens.
#[test]
fn on_a_terminal_nothing_to_show_opens_nothing() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let mut pty = Pty::spawn(&["announcements"], &files.pty_envs());
    let status = pty.exit();
    assert!(status.success(), "{status:?}\n{}", pty.contents());
    assert!(
        pty.contents().contains("› No announcements"),
        "{}",
        pty.contents()
    );
    assert!(
        !pty.contents().contains("╭ Announcements"),
        "{}",
        pty.contents()
    );
    pty.assert_restored();
}

/// How long a PTY test waits for the screen or for sb to exit.
const WAIT: Duration = Duration::from_secs(20);

/// sb on a PTY of 100×30 (portable-pty), its screen parsed through vt100
/// as it arrives: the log screens' harness (`logs_pty`), cut to what
/// these tests use. Why not that module itself: most of it (the
/// `systemctl` world, the open-files limit) would be dead code in this
/// test binary, which the gate's warnings refuse, as `edit.rs` found.
struct Pty {
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    parser: vt100::Parser,
}

impl Pty {
    /// sb with `args`, `common::apply_sb_env`'s environment, then
    /// `TERM=xterm-256color`, `NO_COLOR` and `envs`.
    fn spawn(args: &[&str], envs: &[(&str, String)]) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        let mut base = common::sb();
        let mut cmd = CommandBuilder::new(base.get_program());
        for (k, v) in base.get_envs() {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            }
        }
        base.args(args);
        cmd.args(base.get_args());
        cmd.env("TERM", "xterm-256color");
        cmd.env("NO_COLOR", "1");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn");
        drop(pair.slave);
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
        while !self.contents().contains(text) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.output.recv_timeout(left) {
                Ok(bytes) => self.parser.process(&bytes),
                Err(_) => break,
            }
        }
        self.contents().contains(text)
    }

    /// Wait for sb to exit, reading its output meanwhile.
    fn exit(&mut self) -> ExitStatus {
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

    /// The alternate screen left, mouse capture off, the cursor shown.
    fn assert_restored(&self) {
        let screen = self.parser.screen();
        assert!(!screen.alternate_screen(), "{}", screen.contents());
        assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
        assert!(!screen.hide_cursor());
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Saltbox's and Sandbox's own files, always both (the maintainer,
/// 2026-11-17), Saltbox's first, each entry headed with its repository.
#[test]
fn both_repositories_are_always_shown() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX_ONE);
    files.sandbox_file(SANDBOX);
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(
        stdout(&out),
        format!("{SALTBOX_ONE_SHOWN}\n{SANDBOX_SHOWN}")
    );
    // Sandbox's alone: still headed with its repository.
    std::fs::remove_file(files.path("announcements.yml")).expect("saltbox file");
    let out = files.run(&[]);
    assert_eq!(stdout(&out), SANDBOX_SHOWN, "{}", both(&out));
}

/// No Sandbox checkout: Saltbox's alone, no error and no notice.
#[test]
fn a_missing_sandbox_checkout_shows_saltboxs_alone() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX_ONE);
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), SALTBOX_ONE_SHOWN);
    assert!(
        stderr(&out).contains("› 1 announcement\n"),
        "{}",
        both(&out)
    );
    assert!(
        !stderr(&out).contains("has no announcements file"),
        "{}",
        both(&out)
    );
}

/// An invalid file warns, naming it, and the other repository's
/// announcements show, exit 0 (the maintainer, 2026-11-16; ops decision
/// 31), as `sb update` and a missing checkout do.
#[test]
fn an_invalid_file_warns_and_the_other_shows() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX_ONE);
    files.sandbox_file(INVALID);
    let sandbox = files.path("appdata/sandbox/announcements.yml");
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), SALTBOX_ONE_SHOWN);
    let warning = format!(
        "Sandbox announcements skipped: error loading announcement file '{}': \
         error parsing announcement YAML:",
        sandbox.display()
    );
    assert!(stderr(&out).contains(&warning), "{}", both(&out));
    assert!(
        stderr(&out).contains("› 1 announcement\n"),
        "{}",
        both(&out)
    );
    // Both invalid: two warnings, nothing to show, exit 0.
    files.saltbox_file(INVALID);
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), "");
    for repo in ["Saltbox", "Sandbox"] {
        assert!(
            stderr(&out).contains(&format!("{repo} announcements skipped:")),
            "{repo}: {}",
            both(&out)
        );
    }
    assert!(
        stderr(&out).contains("› No announcements"),
        "{}",
        both(&out)
    );
    // Saltbox's invalid, Sandbox's fine: the same rule.
    files.sandbox_file(SANDBOX);
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), SANDBOX_SHOWN);
    assert!(
        stderr(&out).contains("Saltbox announcements skipped: error loading announcement file"),
        "{}",
        both(&out)
    );
}

/// On a terminal, one list for both repositories, each row labelled with
/// its own; the pane names the selected entry's.
#[test]
fn on_a_terminal_both_repositories_share_the_list() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX_ONE);
    files.sandbox_file(SANDBOX);
    let mut pty = Pty::spawn(&["announcements"], &files.pty_envs());
    assert!(pty.showing("Sandbox news"), "{}", pty.contents());
    let screen = pty.contents();
    assert!(screen.contains("╭ Announcements ─"), "no count: {screen}");
    assert!(
        !screen.contains("● ") && !screen.contains(" NEW "),
        "{screen}"
    );
    assert!(
        screen.contains("2026-04-01  Sandbox  Sandbox news")
            && screen.contains("2026-01-10  Saltbox  Old news"),
        "{screen}"
    );
    // Newest first: Sandbox's, in its pane.
    assert!(screen.contains("╭ Sandbox "), "{screen}");
    pty.send(b"j");
    assert!(pty.showing("╭ Saltbox "), "{}", pty.contents());
    pty.send(b"q");
    assert!(pty.exit().success(), "{}", pty.contents());
    pty.assert_restored();
}

/// A checkout that is there without its announcements file (one older
/// than the file, the maintainer's Sandbox on 2026-11-17) is named in a
/// notice, and the run log records the path read and that the file is
/// missing; a checkout that isn't there says nothing (a server without
/// Sandbox).
#[test]
fn a_checkout_without_its_file_says_so() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    files.saltbox_file(SALTBOX_ONE);
    let out = files.run(&[]);
    assert!(out.status.success(), "{}", both(&out));
    assert!(
        !stderr(&out).contains("has no announcements file"),
        "{}",
        both(&out)
    );
    std::fs::create_dir_all(files.path("appdata/sandbox")).expect("checkout");
    let logs = files.path("logs");
    std::fs::create_dir(&logs).expect("logs");
    let out = files.run_logged(&[], Some(&logs));
    assert!(out.status.success(), "{}", both(&out));
    let sandbox = files.path("appdata/sandbox/announcements.yml");
    let wanted = format!("› Sandbox has no announcements file: {}", sandbox.display());
    assert!(stderr(&out).contains(&wanted), "{wanted}:\n{}", both(&out));
    let log = std::fs::read_to_string(logs.join("sb.log")).unwrap_or_default();
    let read = log
        .lines()
        .find(|line| line.contains("announcements read") && line.contains("repo=\"Sandbox\""))
        .unwrap_or_else(|| panic!("no Sandbox record: {log}"));
    assert!(
        read.contains(&sandbox.display().to_string()) && read.contains("file=\"missing\""),
        "{read}"
    );
}
