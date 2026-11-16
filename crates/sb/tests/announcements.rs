//! `sb announcements` end to end (ops spec, Testing Strategy,
//! "`announcements`"): synthetic before and after files in a temporary
//! directory, which is also the Saltbox root, and the inline output on
//! stdout. Migrations are never listed, asked about or run here (a
//! stand-in `ansible-playbook` that would leave a mark). Root runs
//! (`SB_TEST_ROOT=1`): the command needs root; each piped run has no
//! controlling terminal (`setsid -w`). On a PTY ([`Pty`], as the log
//! screens' harness), the viewer opens and closes.

mod common;
mod root_gate;

use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

const BEFORE: &str = "---\nannouncements:\n  - date: \"2026-01-10\"\n    title: \"Old news\"\n    \
                      message: |\n      Already seen.\n";

/// The before file's entry, two new ones out of date order (one asking for
/// a migration, one with a code block), and one gated by a missing file
/// (`MISSING`, a path in the test's directory: [`Files::new`]).
const AFTER: &str = "---\nannouncements:\n  - date: \"2026-01-10\"\n    title: \"Old news\"\n    \
                     message: |\n      Already seen.\n  - date: \"2026-03-02\"\n    \
                     title: \"Settings moved\"\n    migration:\n      required: true\n      \
                     tag: settings_move\n    message: |\n      ## Settings moved\n\n      \
                     Run the **migration** when asked.\n  - date: \"2026-02-14\"\n    \
                     title: \"New defaults\"\n    message: |\n      ### What changed\n\n      \
                     ```yaml\n      network: saltbox\n      ```\n  - date: \"2026-02-20\"\n    \
                     title: \"Gated\"\n    required_file: \"MISSING\"\n    \
                     message: Not shown.\n";

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("before.yml"), BEFORE).expect("before");
        // The gated entry's file: one that can't exist, inside the
        // directory, so no file on the host decides the test.
        let missing = dir.path().join("missing/file");
        let after = AFTER.replace("MISSING", &missing.display().to_string());
        std::fs::write(dir.path().join("after.yml"), after).expect("after");
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

    /// `sb announcements <args>` with no controlling terminal, the
    /// stand-in as the playbook and on `PATH`.
    fn run(&self, args: &[&str]) -> Output {
        let sb = assert_cmd::cargo::cargo_bin("sb");
        let mut cmd = Command::new("setsid");
        common::apply_sb_env(&mut cmd);
        cmd.env("SB_ANSIBLE_PLAYBOOK", self.path("ansible-playbook"));
        cmd.env("SB_SALTBOX_DIR", self.dir.path());
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs = vec![self.dir.path().to_path_buf()];
        dirs.extend(std::env::split_paths(&path));
        cmd.env("PATH", std::env::join_paths(dirs).expect("PATH"));
        cmd.arg("-w").arg(sb).arg("announcements").args(args);
        cmd.current_dir(self.dir.path());
        cmd.output().expect("run sb")
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

    /// Sandbox's own announcements file: `<appdata>/sandbox`, its
    /// checkout created.
    fn sandbox_file(&self, text: &str) {
        let checkout = self.path("appdata/sandbox");
        std::fs::create_dir_all(&checkout).expect("checkout");
        std::fs::write(checkout.join("announcements.yml"), text).expect("sandbox file");
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

const SHOWN: &str = "# Saltbox Announcement - February 14, 2026\n\n### What changed\n\n\
                     ```yaml\nnetwork: saltbox\n```\n\n\
                     # Saltbox Announcement - March 2, 2026\n\n## Settings moved\n\n\
                     Run the migration when asked.\n";

const TABLE_LINE: &str = "The following tags need to be run to handle migrations:";

/// The new entries inline, oldest first; an entry asking for a migration
/// is shown, but nothing is listed, asked or run: migrations are
/// `sb update`'s.
#[test]
fn it_shows_the_new_entries_and_no_migrations() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let out = files.run(&["-b", "before.yml", "-a", "after.yml"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    let shown = stdout(&out);
    assert_eq!(shown, SHOWN);
    assert!(!shown.contains('\x1b'), "piped: no colour\n{shown}");
    let steps = stderr(&out);
    // The step counts what is shown (the maintainer, 2026-11-16; ops
    // decision 31): piped, the two new ones.
    assert!(steps.contains("› 2 announcements"), "{steps}");
    assert!(steps.contains("Read announcements"), "{steps}");
    assert!(
        !steps.contains(" new ") && !steps.contains("new announcements"),
        "\"new\" is sb update's word: {steps}"
    );
    for text in [TABLE_LINE, "Run all migration tags?", "Would execute"] {
        assert!(!both(&out).contains(text), "{text}: {}", both(&out));
    }
    assert!(!files.ran(), "a playbook ran");
}

/// `--migrations` went with the migrations: a parse error.
#[test]
fn migrations_is_no_flag() {
    let out = common::sb()
        .args(["announcements", "--migrations", "run", "--color=never"])
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(2), "{}", both(&out));
    assert!(stderr(&out).contains("--migrations"), "{}", both(&out));
}

/// No `--before`: every applicable entry is new, for the repository
/// `--repo` names; a missing before file is no file too.
#[test]
fn without_a_before_file_every_entry_is_new() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let expected = "# Sandbox Announcement - January 10, 2026\n\nAlready seen.\n";
    for args in [
        &["-a", "before.yml", "-r", "Sandbox"][..],
        &["-b", "nothing.yml", "-a", "before.yml", "-r", "Sandbox"],
    ] {
        let out = files.run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", both(&out));
        assert_eq!(stdout(&out), expected, "{args:?}");
    }
}

/// No `--after`: the repository's own file, at the Saltbox root
/// (`SB_SALTBOX_DIR`) for Saltbox.
#[test]
fn without_an_after_file_the_repositorys_own_is_read() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    std::fs::write(files.path("announcements.yml"), AFTER).expect("own file");
    let out = files.run(&["-b", "before.yml"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), SHOWN);
}

/// Nothing new: no output on stdout, and the step says there is nothing
/// to show.
#[test]
fn nothing_new_prints_nothing() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    for (before, after) in [("after.yml", "after.yml"), ("before.yml", "nothing.yml")] {
        let out = files.run(&["-b", before, "-a", after]);
        assert_eq!(out.status.code(), Some(0), "{}", both(&out));
        assert_eq!(stdout(&out), "", "{}", both(&out));
        assert!(
            stderr(&out).contains("› No announcements"),
            "{}",
            both(&out)
        );
    }
}

#[test]
fn colour_follows_the_choice() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let out = files.run(&["-b", "before.yml", "-a", "after.yml", "--color=always"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    let shown = stdout(&out);
    assert!(shown.contains("\x1b["), "{shown}");
    assert!(
        !shown.contains("## Settings moved"),
        "markers hidden\n{shown}"
    );
    assert!(shown.contains("Settings moved"), "{shown}");
}

/// An invalid file is an error here, sb-go's words, exit 1.
#[test]
fn an_invalid_file_fails() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    std::fs::write(files.path("bad.yml"), "announcements:\n  - title: [x]\n").expect("bad");
    let out = files.run(&["-b", "bad.yml", "-a", "after.yml"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        stderr(&out).contains(
            "error loading before announcement file 'bad.yml': error parsing announcement YAML: \
             bad.yml:2:12: announcements[0].title: must be a string, not a sequence"
        ),
        "{}",
        both(&out)
    );
    assert_eq!(stdout(&out), "");
}

#[test]
fn an_unknown_repository_is_sb_gos_error() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let out = files.run(&["-b", "before.yml", "-a", "after.yml", "-r", "saltbox"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        stderr(&out).contains("invalid repository name 'saltbox', must be 'Saltbox' or 'Sandbox'"),
        "{}",
        both(&out)
    );
}

/// Shown in `sb --help` now (the maintainer, 2026-11-15); its own help
/// has sb-go's flags, but no `-v` and no `--migrations`.
#[test]
fn it_is_shown_with_sb_gos_flags() {
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
    for flag in ["--before", "--after", "--repo"] {
        assert!(help.contains(flag), "{flag}: {help}");
    }
    for flag in ["--verbose", "--migrations"] {
        assert!(!help.contains(flag), "{flag}: {help}");
    }
    assert!(
        help.contains("Display Saltbox's and Sandbox's announcements")
            && help.contains("the new ones marked"),
        "{help}"
    );
}

/// On a terminal, the viewer: every entry that applies, the new ones
/// marked; `q` closes it, exit 0, the terminal restored.
#[test]
fn on_a_terminal_the_viewer_lists_every_entry_and_q_closes_it() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let logs = files.path("logs");
    std::fs::create_dir(&logs).expect("logs");
    let mut envs = files.pty_envs();
    envs.push(("SB_LOG_DIR", logs.display().to_string()));
    let before = files.path("before.yml").display().to_string();
    let after = files.path("after.yml").display().to_string();
    let mut pty = Pty::spawn(&["announcements", "-b", &before, "-a", &after], &envs);
    assert!(pty.showing("Announcements · 2 new"), "{}", pty.contents());
    let screen = pty.contents();
    for text in ["Settings moved", "New defaults", "Old news", " NEW "] {
        assert!(screen.contains(text), "{text}: {screen}");
    }
    assert!(!screen.contains("Gated"), "{screen}");
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{status:?}\n{}", pty.contents());
    pty.assert_restored();
    // The step counted what the viewer showed: all three that apply.
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
    let envs = files.pty_envs();
    let after = files.path("after.yml").display().to_string();
    let mut pty = Pty::spawn(&["announcements", "-a", &after], &envs);
    assert!(pty.showing("Announcements · 3 new"), "{}", pty.contents());
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
    let envs = files.pty_envs();
    let mut pty = Pty::spawn(&["announcements"], &envs);
    let status = pty.exit();
    assert!(status.success(), "{status:?}\n{}", pty.contents());
    assert!(
        pty.contents().contains("› No announcements"),
        "{}",
        pty.contents()
    );
    assert!(
        !pty.contents().contains("Announcements ·"),
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

/// Sandbox's file for the both-repositories tests: one entry.
const SANDBOX: &str = "announcements:\n  - date: \"2026-04-01\"\n    title: \"Sandbox news\"\n    \
                       message: From the sandbox.\n";

const SALTBOX_OWN: &str = "# Saltbox Announcement - January 10, 2026\n\nAlready seen.\n";
const SANDBOX_OWN: &str = "# Sandbox Announcement - April 1, 2026\n\nFrom the sandbox.\n";

/// No `-r`: Saltbox's and Sandbox's own files, Saltbox's first (ops
/// decision 30); `-r` narrows to the one named.
#[test]
fn both_repositories_by_default_and_r_narrows_to_one() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    std::fs::write(files.path("announcements.yml"), BEFORE).expect("saltbox file");
    files.sandbox_file(SANDBOX);
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), format!("{SALTBOX_OWN}\n{SANDBOX_OWN}"));
    for (repo, shown) in [("Saltbox", SALTBOX_OWN), ("Sandbox", SANDBOX_OWN)] {
        let out = files.run(&["-r", repo]);
        assert_eq!(out.status.code(), Some(0), "{repo}: {}", both(&out));
        assert_eq!(stdout(&out), shown, "{repo}");
    }
}

/// No Sandbox checkout: Saltbox's alone, no error.
#[test]
fn a_missing_sandbox_checkout_shows_saltboxs_alone() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    std::fs::write(files.path("announcements.yml"), BEFORE).expect("saltbox file");
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), SALTBOX_OWN);
    assert!(
        stderr(&out).contains("› 1 announcement\n"),
        "{}",
        both(&out)
    );
    // `-r Sandbox` there: nothing, no error either.
    let out = files.run(&["-r", "Sandbox"]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), "");
    assert!(
        stderr(&out).contains("› No announcements"),
        "{}",
        both(&out)
    );
}

/// Without `-r`, an invalid file the user didn't name warns, naming it,
/// and the other repository's announcements show, exit 0 (the
/// maintainer, 2026-11-16; ops decision 31), as `sb update` and a missing
/// checkout do; with `-r` naming that repository, it still fails, exit 1.
#[test]
fn an_invalid_file_sb_found_itself_warns_and_the_other_shows() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    let invalid = "announcements:\n  - title: [x]\n";
    std::fs::write(files.path("announcements.yml"), BEFORE).expect("saltbox file");
    files.sandbox_file(invalid);
    let sandbox = files.path("appdata/sandbox/announcements.yml");
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), SALTBOX_OWN);
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
    // Named with -r: an error, as before.
    let out = files.run(&["-r", "Sandbox"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        stderr(&out).contains(&format!(
            "error loading after announcement file '{}'",
            sandbox.display()
        )),
        "{}",
        both(&out)
    );
    assert_eq!(stdout(&out), "");
    // A file named with --after, without -r: an error, as before.
    std::fs::write(files.path("bad.yml"), invalid).expect("bad");
    let out = files.run(&["-a", "bad.yml"]);
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        stderr(&out).contains("error loading after announcement file 'bad.yml'"),
        "{}",
        both(&out)
    );
    // Both found invalid: two warnings, nothing to show, exit 0, as
    // `sb update` skips each (decision 31).
    std::fs::write(files.path("announcements.yml"), invalid).expect("saltbox file");
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
    // Saltbox's own file invalid, Sandbox's fine: the same rule.
    std::fs::write(files.path("announcements.yml"), invalid).expect("saltbox file");
    files.sandbox_file(SANDBOX);
    let out = files.run(&[]);
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(stdout(&out), SANDBOX_OWN);
    assert!(
        stderr(&out).contains("Saltbox announcements skipped: error loading announcement file"),
        "{}",
        both(&out)
    );
}

/// On a terminal, one list for both repositories; the pane names the
/// selected entry's.
#[test]
fn on_a_terminal_both_repositories_share_the_list() {
    if !root_gate::requested() {
        return;
    }
    let files = Files::new();
    std::fs::write(files.path("announcements.yml"), BEFORE).expect("saltbox file");
    files.sandbox_file(SANDBOX);
    let mut pty = Pty::spawn(&["announcements"], &files.pty_envs());
    assert!(pty.showing("Announcements · 2 new"), "{}", pty.contents());
    let screen = pty.contents();
    assert!(
        screen.contains("Sandbox news") && screen.contains("Old news"),
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
