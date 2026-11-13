//! `sb logs` through the built binary (tui spec, Testing Strategy, "End
//! to end"): a stand-in `systemctl` first on `PATH`, crafted journal
//! files through `SB_JOURNAL_DIR`, the run log in the test's directory.
//! Runs before the screen are piped; the screen runs on a PTY
//! (portable-pty), read through vt100: the picker's rows, Enter opens a
//! service's log, an appended entry appears while following, Esc back to
//! the picker, `q` exits 0, Ctrl-C exits 130, the terminal restored
//! (alternate screen left, mouse off, the PTY's termios read back in
//! canonical mode with echo), and no log content in the run log.

mod common;

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use assert_cmd::prelude::*;
use portable_pty::{
    Child, CommandBuilder, ExitStatus, MasterPty, PtySize, SlavePty, native_pty_system,
};
use rustix::fs::{Mode, OFlags, open};
use rustix::termios::{LocalModes, tcgetattr};
use sb_host::journal::testing::crafted::{
    Crafted, D_ENTRY_ARRAY, D_ENTRY_OFFSET, D_N_ENTRIES, H_ARENA_SIZE, H_ENTRY_ARRAY_OFFSET,
    H_N_ENTRIES,
};

const WAIT: Duration = Duration::from_secs(20);

/// The unit whose log the tests open.
const APP: &str = "saltbox_managed_app";

/// A string the run log must never hold: it is only in a log's content.
const MARKER: &str = "content-marker-7f3a";

/// `list-units`: two Saltbox units and docker; another unit the default
/// filters leave out.
const UNITS: &str = r#"[
{"unit":"saltbox_managed_app.service","load":"loaded","active":"active","sub":"running","description":"app"},
{"unit":"saltbox_managed_backup.service","load":"loaded","active":"failed","sub":"failed","description":"backup"},
{"unit":"docker.service","load":"loaded","active":"active","sub":"running","description":"Docker"},
{"unit":"ssh.service","load":"loaded","active":"active","sub":"running","description":"ssh"}
]"#;

const UNIT_FILES: &str = r#"[
{"unit_file":"saltbox_managed_app.service","state":"enabled","vendor_preset":null},
{"unit_file":"saltbox_managed_backup.service","state":"enabled","vendor_preset":null},
{"unit_file":"docker.service","state":"enabled","vendor_preset":null}
]"#;

/// One test's world: the stand-in `systemctl`, the journal directory and
/// the run log's directory.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    /// A world whose `systemctl` answers each listing with the file of
    /// its name, and `show` with nothing (no times).
    fn new(units: &str, unit_files: &str) -> Self {
        let world = Self::with_script(
            "case \"$1\" in\n\
             list-units) cat \"$WORLD/units.json\" ;;\n\
             list-unit-files) cat \"$WORLD/files.json\" ;;\n\
             list-timers) echo '[]' ;;\n\
             show) ;;\n\
             *) exit 9 ;;\n\
             esac\n",
        );
        std::fs::write(world.dir.path().join("units.json"), units).expect("units");
        std::fs::write(world.dir.path().join("files.json"), unit_files).expect("files");
        world
    }

    /// A world whose `systemctl` is `script`, `$WORLD` its directory.
    fn with_script(script: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let program = bin.join("systemctl");
        let world = dir.path().display().to_string();
        std::fs::write(&program, format!("#!/bin/sh\nWORLD='{world}'\n{script}"))
            .expect("systemctl");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::create_dir(dir.path().join("journal")).expect("journal");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        Self { dir }
    }

    fn journal(&self) -> PathBuf {
        self.dir.path().join("journal")
    }

    /// The stand-in first on `PATH`, then the system's.
    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.dir.path().join("bin").display())
    }

    /// The environment of every run: `common::apply_sb_env`'s, then the
    /// world's.
    fn envs(&self) -> Vec<(String, Option<String>)> {
        let mut base = std::process::Command::new("sb");
        common::apply_sb_env(&mut base);
        let mut envs: Vec<(String, Option<String>)> = base
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for (k, v) in [
            (
                "SB_LOG_DIR",
                self.dir.path().join("logs").display().to_string(),
            ),
            ("SB_JOURNAL_DIR", self.journal().display().to_string()),
            ("PATH", self.path()),
        ] {
            envs.push((k.to_owned(), Some(v)));
        }
        envs
    }

    /// `sb logs`, piped.
    fn piped(&self) -> Output {
        let mut cmd = common::sb();
        cmd.arg("logs").env("NO_COLOR", "1");
        for (k, v) in self.envs() {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        cmd.output().expect("run sb")
    }

    /// The run log's text.
    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("logs/sb.log")).unwrap_or_default()
    }

    /// `APP`'s journal: three entries of its own and one of another unit.
    fn write_journal(&self) {
        let unit = format!("_SYSTEMD_UNIT={APP}.service");
        let marker = format!("MESSAGE=a line with {MARKER}");
        let entries: [&[&[u8]]; 4] = [
            &[
                b"MESSAGE=hello from app",
                unit.as_bytes(),
                b"SYSLOG_IDENTIFIER=app",
                b"SYSLOG_PID=42",
            ],
            &[b"MESSAGE=not this unit", b"_SYSTEMD_UNIT=docker.service"],
            &[
                marker.as_bytes(),
                unit.as_bytes(),
                b"SYSLOG_IDENTIFIER=app",
                b"SYSLOG_PID=42",
            ],
            &[
                b"MESSAGE=ready",
                unit.as_bytes(),
                b"SYSLOG_IDENTIFIER=app",
                b"SYSLOG_PID=42",
            ],
        ];
        journal_file(&self.journal().join("system.journal"), 1, &entries);
    }

    /// A second file with one more entry, as journald's next file would
    /// come (renamed into place, so the follow's watch sees it whole).
    fn append(&self) {
        let unit = format!("_SYSTEMD_UNIT={APP}.service");
        let entries: [&[&[u8]]; 1] = [&[
            b"MESSAGE=appended while following",
            unit.as_bytes(),
            b"SYSLOG_IDENTIFIER=app",
            b"SYSLOG_PID=42",
        ]];
        let part = self.dir.path().join("next.part");
        journal_file(&part, 10, &entries);
        std::fs::rename(&part, self.journal().join("user-1000.journal")).expect("rename");
    }
}

/// A journal file at `path` of one entry per element of `entries`, each
/// carrying the payloads given, seqnums from `first`, one second apart
/// and ten minutes ago: sb-journal's crafted builder, as its
/// `journal_of` lays the objects out, with times set.
fn journal_file(path: &Path, first: u64, entries: &[&[&[u8]]]) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after 1970")
        .as_secs();
    let base = (now - 600) * 1_000_000;
    let mut c = Crafted::new();
    c.hash_table(7);
    let main = c.array(&[], entries.len(), 0);
    let mut objects: Vec<(Vec<u8>, u64)> = Vec::new();
    let mut items_of = Vec::new();
    for payloads in entries {
        let mut items = Vec::new();
        for &payload in *payloads {
            let d = if let Some(&(_, d)) = objects.iter().find(|(p, _)| p == payload) {
                d
            } else {
                let d = c.data(payload);
                objects.push((payload.to_vec(), d));
                d
            };
            items.push(d);
        }
        items_of.push(items);
    }
    let offsets: Vec<u64> = items_of
        .iter()
        .zip(first..)
        .map(|(items, seqnum)| {
            let e = c.entry(seqnum, items);
            // The entry's realtime, after the object header and seqnum.
            c.set(e + 24, base + seqnum * 1_000_000);
            e
        })
        .collect();
    for (k, &e) in (0..).zip(&offsets) {
        c.set_array_item(main, k, e);
    }
    for &(_, d) in &objects {
        let listing: Vec<u64> = offsets
            .iter()
            .zip(&items_of)
            .filter(|(_, items)| items.contains(&d))
            .map(|(&e, _)| e)
            .collect();
        c.set(d + D_ENTRY_OFFSET, listing[0]);
        c.set(d + D_N_ENTRIES, listing.len() as u64);
        let rest = c.array(&listing[1..], 0, 0);
        c.set(d + D_ENTRY_ARRAY, rest);
        if listing.len() > 1 {
            c.set_tail(d, rest, listing.len() as u64 - 1);
        }
    }
    c.set(H_N_ENTRIES, offsets.len() as u64);
    c.set(H_ENTRY_ARRAY_OFFSET, main);
    c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
    std::fs::write(path, &c.b).expect("journal file");
}

/// `sb logs` on a PTY of 100×30, its screen parsed as it arrives.
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
    fn spawn(world: &World) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        let mut cmd = CommandBuilder::new(common::sb().get_program());
        cmd.arg("logs");
        for (k, v) in world.envs() {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("NO_COLOR", "1");
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

    /// The terminal is restored: the alternate screen left, mouse capture
    /// off, the cursor shown, and the PTY back in canonical mode with echo.
    fn assert_restored(&self) {
        let screen = self.parser.screen();
        assert!(!screen.alternate_screen(), "{}", screen.contents());
        assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
        assert!(!screen.hide_cursor());
        let tty = open(&self.tty, OFlags::RDONLY | OFlags::NOCTTY, Mode::empty()).expect("tty");
        let modes = tcgetattr(&tty).expect("termios").local_modes;
        assert!(
            modes.contains(LocalModes::ICANON | LocalModes::ECHO),
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

/// sb-go's short text stays (the command contract); the long one
/// describes the picker (tui spec, "Help texts").
#[test]
fn help_is_pinned() {
    let help = common::sb()
        .args(["logs", "--help", "--color=never"])
        .assert()
        .success();
    insta::assert_snapshot!(
        "logs_help",
        String::from_utf8_lossy(&help.get_output().stdout)
    );
    common::sb().args(["logs", "x"]).assert().code(2);
}

/// No services: sb-go's message, extended to both kinds, exit 0, before
/// any screen (so piped too).
#[test]
fn no_services_print_the_message_and_exit_0() {
    let world = World::new("[]", "[]");
    let out = world.piped();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "No matching systemd services or Docker containers found.\n"
    );
    assert!(out.status.success(), "{out:?}");
}

/// A failing `systemctl`: host's error in the error block, exit 1.
#[test]
fn a_failing_service_list_is_an_error() {
    let world = World::with_script("echo 'Failed to connect to bus' >&2\nexit 1\n");
    let out = world.piped();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("failed to list systemd services"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

/// Services but no terminal: the error, exit 1, nothing on stdout.
#[test]
fn no_terminal_is_an_error_with_nothing_on_stdout() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let out = world.piped();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sb logs needs a terminal to choose a log"),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
}

/// A journal directory that can't be read: the error block before the
/// screen, exit 1.
#[test]
fn an_unreadable_journal_is_an_error_before_the_screen() {
    let world = World::new(UNITS, UNIT_FILES);
    std::fs::remove_dir(world.journal()).expect("no journal");
    let mut pty = Pty::spawn(&world);
    let status = pty.exit();
    assert_eq!(status.exit_code(), 1, "{}", pty.contents());
    assert!(
        pty.contents().contains("read the journal directory"),
        "{}",
        pty.contents()
    );
    assert!(!pty.parser.screen().alternate_screen());
}

/// The picker lists the Saltbox services, Enter opens one's log, a new
/// entry appears while following, Esc goes back, `q` exits 0 with the
/// terminal restored; the run log has the source and none of its
/// content.
#[test]
fn the_picker_opens_a_services_log_which_follows() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    let screen = pty.contents();
    assert!(screen.contains("› docker"), "{screen}");
    assert!(screen.contains("saltbox_managed_app"), "{screen}");
    assert!(screen.contains("✗ failed"), "{screen}");
    assert!(!screen.contains("ssh"), "{screen}");
    assert!(pty.parser.screen().alternate_screen());
    // The second row is the app.
    pty.send(b"j\r");
    assert!(pty.showing("app[42]: hello from app"), "{}", pty.contents());
    assert!(pty.showing("--- following ---"), "{}", pty.contents());
    let screen = pty.contents();
    assert!(screen.contains("app[42]: ready"), "{screen}");
    assert!(!screen.contains("not this unit"), "{screen}");
    world.append();
    assert!(
        pty.showing("appended while following"),
        "{}",
        pty.contents()
    );
    pty.send(b"\x1b");
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{status:?}");
    pty.assert_restored();
    let log = world.log();
    assert!(log.contains("source opened"), "{log}");
    assert!(log.contains(APP), "{log}");
    assert!(!log.contains(MARKER), "{log}");
}

/// Ctrl-C in the viewer: exit 130, quietly, the terminal restored.
#[test]
fn ctrl_c_exits_130_with_the_terminal_restored() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"j\r");
    assert!(pty.showing("hello from app"), "{}", pty.contents());
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    pty.assert_restored();
    assert!(!pty.contents().contains("ERROR"), "{}", pty.contents());
}

/// Ctrl-C in the picker: exit 130 too.
#[test]
fn ctrl_c_in_the_picker_exits_130() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{}", pty.contents());
    pty.assert_restored();
}

/// `q` in the viewer quits sb, exit 0, rather than going back.
#[test]
fn q_in_the_viewer_quits() {
    let world = World::new(UNITS, UNIT_FILES);
    world.write_journal();
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Services (3)"), "{}", pty.contents());
    pty.send(b"j\r");
    assert!(pty.showing("hello from app"), "{}", pty.contents());
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{status:?}");
    pty.assert_restored();
}

/// A dotted service name opens its own log (sb-host lists it without
/// `.service`).
#[test]
fn a_dotted_service_name_opens_its_log() {
    let units = r#"[{"unit":"saltbox_managed_a.b.service","load":"loaded","active":"active","sub":"running","description":"a.b"}]"#;
    let files =
        r#"[{"unit_file":"saltbox_managed_a.b.service","state":"enabled","vendor_preset":null}]"#;
    let world = World::new(units, files);
    let unit = "_SYSTEMD_UNIT=saltbox_managed_a.b.service";
    let entries: [&[&[u8]]; 1] = [&[b"MESSAGE=dotted unit line", unit.as_bytes()]];
    journal_file(&world.journal().join("system.journal"), 1, &entries);
    let mut pty = Pty::spawn(&world);
    assert!(pty.showing("Services (1)"), "{}", pty.contents());
    pty.send(b"\r");
    assert!(pty.showing("dotted unit line"), "{}", pty.contents());
    pty.send(b"q");
    assert!(pty.exit().success());
}
