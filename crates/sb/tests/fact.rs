//! `sb fact` through the binary (tui spec, "Editors", `sb fact`; module
//! build M54): the refusal without a terminal, the open errors before
//! the screen, the page on a PTY over fixture fact files, `q` and Ctrl-C,
//! an edit saved, and the run log without a fact's value. Every test has its own Saltbox
//! root, whose inventory points `server_appdata_path` at its own facts
//! directory; none reads a real one.

mod common;
mod root_gate;

use std::io::{Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, SlavePty, native_pty_system};

/// A known secret in the fixture: never in the run log.
const SECRET: &str = "not-a-real-password";

/// A test's world: a Saltbox root whose inventory names `appdata`, the
/// facts directory under it, and a log directory.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    /// A world with proto-facts' synthetic apps.
    fn new() -> Self {
        let world = Self::empty();
        std::fs::create_dir(world.facts()).expect("facts");
        world.write(
            "homepage.ini",
            "[homepage]\ntitle = Home\nmotd = first line\n\tsecond line\n",
        );
        world.write(
            "komodo.ini",
            &format!("# Synthetic.\n[komodo]\ndatabase_password = {SECRET}\n"),
        );
        world.write(
            "plex.ini",
            "[plex]\nclaim_token = claim-FAKE0\n\n[plex2]\nclaim_token = claim-FAKE1\n",
        );
        world
    }

    /// A world with no facts directory.
    fn empty() -> Self {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("temp dir");
        let inventory = dir.path().join("saltbox/inventories/host_vars");
        std::fs::create_dir_all(&inventory).expect("inventory");
        std::fs::create_dir(dir.path().join("appdata")).expect("appdata");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        std::fs::write(
            inventory.join("localhost.yml"),
            format!(
                "server_appdata_path: {}\n",
                dir.path().join("appdata").display()
            ),
        )
        .expect("inventory");
        Self { dir }
    }

    fn facts(&self) -> PathBuf {
        self.dir.path().join("appdata/saltbox")
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.facts().join(name), text).expect("fact file");
    }

    fn logs(&self) -> PathBuf {
        self.dir.path().join("logs")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.logs().join("sb.log")).unwrap_or_default()
    }

    /// `sb fact` in this world.
    fn sb(&self) -> Command {
        let mut cmd = common::sb();
        cmd.arg("fact")
            .env("SB_SALTBOX_DIR", self.dir.path().join("saltbox"))
            .env("SB_LOG_DIR", self.logs());
        cmd
    }
}

/// How long a PTY test waits for the screen or for sb to exit.
const WAIT: Duration = Duration::from_secs(20);

/// `sb fact` on a PTY of 100×30 (portable-pty), its screen parsed through
/// vt100 as it arrives. Why not edit.rs's or `logs_pty`'s harness: each
/// is its own test crate's, with that crate's world (as edit.rs says).
struct Pty {
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    _slave: Box<dyn SlavePty + Send>,
    writer: Box<dyn Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    parser: vt100::Parser,
    /// Whether the alternate screen was ever entered.
    drew: bool,
}

impl Pty {
    fn spawn(world: &World) -> Self {
        Self::spawn_with(world, None)
    }

    /// [`Pty::spawn`] with sb started by `/bin/sh` as `exec sb fact
    /// <redirect>`, so one of its streams is something other than the PTY.
    fn spawn_with(world: &World, redirect: Option<&str>) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        // The environment `common::sb()` gave it (`common::apply_sb_env(..)`)
        // and the world's, read back from that `Command`.
        let base = world.sb();
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
            writer,
            output,
            parser: vt100::Parser::new(30, 100, 0),
            drew: false,
        }
    }

    fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.drew |= self.parser.screen().alternate_screen();
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
                Ok(bytes) => self.process(&bytes),
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
                Ok(bytes) => self.process(&bytes),
                Err(_) => return done(self.parser.screen()),
            }
        }
        true
    }

    /// Take the output already here, without waiting.
    fn take_now(&mut self) {
        while let Ok(bytes) = self.output.try_recv() {
            self.process(&bytes);
        }
    }

    /// Wait for sb to exit, reading its output meanwhile.
    fn exit(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                while let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(200)) {
                    self.process(&bytes);
                }
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "sb didn't exit:\n{}",
                self.contents()
            );
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
                self.process(&bytes);
            }
        }
    }

    /// The terminal is restored: no alternate screen or mouse capture
    /// left, the cursor shown.
    fn assert_restored(&self) {
        let screen = self.parser.screen();
        assert!(!screen.alternate_screen(), "{}", self.contents());
        assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
        assert!(!screen.bracketed_paste(), "{}", self.contents());
        assert!(!screen.hide_cursor(), "{}", self.contents());
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Without a terminal on stdin and stdout, sb fact refuses before it opens
/// anything: no editor lock is created.
#[test]
fn without_a_terminal_it_refuses_before_opening_anything() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let output = world.sb().output().expect("run sb");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("sb fact requires an interactive terminal for input and output"),
        "{stderr}"
    );
    assert!(!world.facts().join(".fact-editor.lock").exists());
}

/// Both stdin and stdout must be terminals: either one redirected away
/// from the PTY refuses, before anything is opened.
#[test]
fn with_only_one_stream_on_a_terminal_it_refuses() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let out = world.dir.path().join("out");
    for redirect in [format!("> '{}'", out.display()), "< /dev/null".to_owned()] {
        let mut pty = Pty::spawn_with(&world, Some(&redirect));
        let status = pty.exit();
        assert_eq!(status.exit_code(), 1, "{redirect}: {}", pty.contents());
        let shown = format!(
            "{}{}",
            pty.contents(),
            std::fs::read_to_string(&out).unwrap_or_default()
        );
        assert!(
            shown.contains("sb fact requires an interactive terminal for input and output"),
            "{redirect}: {shown}"
        );
        assert!(!pty.drew, "{redirect}: the screen was drawn");
        assert!(
            !world.facts().join(".fact-editor.lock").exists(),
            "{redirect}"
        );
    }
}

/// SIGTERM on the page: the terminal restored, sb ends by the signal (143
/// to a shell).
#[test]
fn sigterm_restores_the_terminal() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Saltbox settings"), "{}", pty.contents());
    let pid = pty
        .child
        .process_id()
        .and_then(|pid| rustix::process::Pid::from_raw(i32::try_from(pid).ok()?))
        .expect("sb's pid");
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).expect("kill");
    let status = pty.exit();
    assert_eq!(
        status.signal(),
        Some("Terminated"),
        "{status:?}\n{}",
        pty.contents()
    );
    pty.assert_restored();
}

/// `sb fact --help` is sb-go's text with the changes panel's sentence
/// (tui spec, difference 28).
#[test]
fn the_help_is_sb_go_s_with_the_changes_panel() {
    let output = common::sb()
        .args(["fact", "--help"])
        .output()
        .expect("run sb");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for line in [
        "Interactively browse and edit existing Saltbox configuration facts.",
        "Changes are listed in the changes panel until you save. Requires an interactive terminal.",
        "Hold Shift while dragging for native",
    ] {
        assert!(text.contains(line), "{line}: {text}");
    }
    assert!(!text.contains("staged for review"), "{text}");
}

/// An open error is sb's plain error block, exit 1, before the screen
/// draws.
fn assert_open_error(world: &World, cause: &str) {
    let mut pty = Pty::spawn(world);
    let status = pty.exit();
    assert_eq!(status.exit_code(), 1, "{}", pty.contents());
    assert!(
        pty.contents()
            .contains(&format!("open fact editor: {cause}")),
        "{}",
        pty.contents()
    );
    assert!(!pty.drew, "the screen was drawn");
    pty.assert_restored();
}

#[test]
fn a_missing_facts_directory_fails_the_open() {
    if !root_gate::requested() {
        return;
    }
    assert_open_error(
        &World::empty(),
        "facts directory must be an existing directory without symlinks",
    );
}

/// A file that doesn't parse fails the open, naming it.
#[test]
fn a_broken_fact_file_fails_the_open_naming_it() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.write("sonarr.ini", "[sonarr]\nport = 1\rx\n");
    assert_open_error(&world, "catalog sonarr:");
}

/// Another fact editor (sb's or sb-go's: the same lock) holds the
/// directory.
#[test]
fn a_second_editor_is_refused() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let lock = std::fs::File::create(world.facts().join(".fact-editor.lock")).expect("lock");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).expect("flock");
    assert_open_error(&world, "another fact editor is active");
}

/// The page over the fixture: every app and its settings, the secret's
/// value as it is (the maintainer, 2026-11-18); `q` quits with exit 0, the terminal restored and the editor
/// lock released.
#[test]
fn the_page_shows_the_facts_and_q_quits() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Saltbox settings"), "{}", pty.contents());
    assert!(pty.showing("claim_token"), "{}", pty.contents());
    let page = pty.contents();
    for text in [
        "▾ Homepage",
        "motd",
        "first line …",
        "▾ Komodo",
        "database_password",
        SECRET,
        "▾ Plex",
        "plex2",
        "Unsaved changes (0)",
        "Quit (q)",
    ] {
        assert!(page.contains(text), "{text}: {page}");
    }
    assert!(!page.contains('•'), "{page}");
    pty.send(b"q");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 0, "{}", pty.contents());
    pty.assert_restored();
    // The lock is free again: a second session could take it.
    let lock = std::fs::File::open(world.facts().join(".fact-editor.lock")).expect("lock");
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .expect("the editor lock released");
}

/// Ctrl-C on the page: exit 130, quietly, the terminal restored.
#[test]
fn ctrl_c_exits_130() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Saltbox settings"), "{}", pty.contents());
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    pty.assert_restored();
}

/// Copy on the secret: the screen shows it, the run log names the fact
/// and never holds its value.
#[test]
fn a_shown_and_copied_secret_never_reaches_the_run_log() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("database_password"), "{}", pty.contents());
    // Homepage's heading and two settings, Komodo's heading, then its
    // password.
    pty.send(b"jjjj");
    assert!(pty.showing(SECRET), "{}", pty.contents());
    pty.send(b"c");
    assert!(
        pty.showing("Copied database_password"),
        "{}",
        pty.contents()
    );
    pty.send(b"q");
    assert_eq!(pty.exit().exit_code(), 0, "{}", pty.contents());
    let log = world.log();
    assert!(log.contains("fact copied"), "{log}");
    assert!(log.contains("key=database_password"), "{log}");
    assert!(!log.contains(SECRET), "{log}");
}

/// An edit saved through the binary: the file holds the new value, its
/// other lines as they were, and the run log names the change, never the
/// value (M55).
#[test]
fn an_edit_is_saved_to_the_file() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Homepage"), "{}", pty.contents());
    // Homepage's heading, motd, then title.
    pty.send(b"jj");
    pty.send(b"e");
    assert!(pty.showing("Edit title"), "{}", pty.contents());
    pty.send(b"\x7f\x7f\x7f\x7fAway");
    pty.send(b"\r");
    assert!(pty.showing("Away  changed"), "{}", pty.contents());
    pty.send(b"v");
    assert!(pty.showing("Save 1 change"), "{}", pty.contents());
    pty.send(b"y");
    assert!(pty.showing("homepage: saved"), "{}", pty.contents());
    pty.send(b"\r");
    pty.send(b"q");
    assert_eq!(pty.exit().exit_code(), 0, "{}", pty.contents());
    pty.assert_restored();
    assert_eq!(
        std::fs::read_to_string(world.facts().join("homepage.ini")).expect("file"),
        "[homepage]\ntitle = Away\nmotd = first line\n\tsecond line\n"
    );
    let log = world.log();
    assert!(log.contains("change staged"), "{log}");
    assert!(
        log.contains("outcome=\"applied\"") || log.contains("outcome=applied"),
        "{log}"
    );
    assert!(!log.contains("Away"), "{log}");
}

/// The facts directory follows the inventory's `server_appdata_path`.
#[test]
fn the_facts_directory_is_under_the_inventory_s_appdata() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Homepage"), "{}", pty.contents());
    // The editor lock is in that directory while the page shows.
    assert!(Path::new(&world.facts().join(".fact-editor.lock")).exists());
    pty.send(b"q");
    assert_eq!(pty.exit().exit_code(), 0);
}

/// An SGR mouse report: a left press and release at `column`, `row`
/// (1-based), as a terminal sends them with capture on.
fn left_click(column: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").into_bytes()
}

/// The smoke test (tui spec, editors, Testing Strategy, "PTY smoke"):
/// open, search, a click, an edit, the save and `q` against fixtures; the
/// file holds the intended bytes, and mouse reports sent while the screen
/// leaves never reach the shell.
#[test]
fn the_smoke_test_searches_edits_saves_and_quits() {
    if !root_gate::requested() {
        return;
    }
    // A run where the harness was too slow to send the reports while sb
    // was still leaving proves nothing either way; it runs again.
    for _ in 0..3 {
        let world = World::new();
        let mut pty = Pty::spawn(&world);
        assert!(pty.showing("Homepage"), "{}", pty.contents());
        // Search for the title, take the keys back, then click its row
        // (the list's first row is the screen's fourth: the search box
        // takes three) and click again to edit it.
        pty.send(b"/titl\r");
        assert!(pty.showing("Search  titl"), "{}", pty.contents());
        let click = left_click(10, 5);
        pty.send(&click);
        pty.send(&click);
        assert!(pty.showing("Edit title"), "{}", pty.contents());
        pty.send(b"\x7f\x7f\x7f\x7fAway\r");
        assert!(pty.showing("Away  changed"), "{}", pty.contents());
        pty.send(b"vy");
        assert!(pty.showing("homepage: saved"), "{}", pty.contents());
        pty.send(b"\rq");
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
        let late = !pty.parser.screen().alternate_screen();
        assert_eq!(pty.exit().exit_code(), 0, "{}", pty.contents());
        pty.assert_restored();
        assert_eq!(
            std::fs::read_to_string(world.facts().join("homepage.ini")).expect("file"),
            "[homepage]\ntitle = Away\nmotd = first line\n\tsecond line\n"
        );
        if late {
            continue;
        }
        // Back in cooked mode the terminal would echo them.
        assert!(!pty.contents().contains("<35;10;5M"), "{}", pty.contents());
        return;
    }
    panic!("the harness never sent the reports while sb left");
}

/// A paste through the terminal (bracketed paste) into the edit box: its
/// newline doesn't stage the value, and the `q` after it isn't a key.
#[test]
fn a_bracketed_paste_goes_into_the_edit_box_whole() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Homepage"), "{}", pty.contents());
    assert!(pty.parser.screen().bracketed_paste());
    // Homepage's heading, motd, then title.
    pty.send(b"jje");
    assert!(pty.showing("Edit title"), "{}", pty.contents());
    pty.send(b"\x1b[200~-a\nq\x1b[201~");
    assert!(pty.showing("Home-a"), "{}", pty.contents());
    pty.send(b"\r");
    assert!(pty.showing("Home-a …  changed"), "{}", pty.contents());
    pty.send(b"q");
    assert!(
        pty.showing("You have 1 unsaved change"),
        "{}",
        pty.contents()
    );
    pty.send(b"d");
    assert_eq!(pty.exit().exit_code(), 0, "{}", pty.contents());
    pty.assert_restored();
}
