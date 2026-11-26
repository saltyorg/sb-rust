//! Shared by `sb logs`' end-to-end tests (`logs.rs` and its
//! `logs_containers` module): a test's world (a stand-in `systemctl`, the journal
//! and run log directories, the Docker daemon), and sb on a PTY of 100×30
//! (portable-pty), its screen parsed through vt100 as it arrives.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Output;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use portable_pty::{
    Child, CommandBuilder, ExitStatus, MasterPty, PtySize, SlavePty, native_pty_system,
};
use rustix::fs::{Mode, OFlags, open};
use rustix::termios::{LocalModes, tcgetattr};
use sb_host::journal::testing::follow_slot::{self, Slot};

use crate::common;

/// How long a test waits for the screen or for sb to exit.
pub const WAIT: Duration = Duration::from_secs(20);

/// One test's world: the stand-in `systemctl`, the journal directory, the
/// run log's directory, and the Docker daemon (by default none: the test
/// environment's socket that doesn't exist).
pub struct World {
    pub dir: tempfile::TempDir,
    docker_host: Option<String>,
    /// The follow slot of a world whose service's log is followed
    /// ([`World::follows`]).
    follow: OnceLock<Slot>,
}

impl World {
    /// A world whose `systemctl` answers each listing with the file of
    /// its name, and `show` with nothing (no times).
    pub fn new(units: &str, unit_files: &str) -> Self {
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
    pub fn with_script(script: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let program = bin.join("systemctl");
        let world = dir.path().display().to_string();
        sb_cli_core::testing::write_program(
            &program,
            format!("#!/bin/sh\nWORLD='{world}'\n{script}"),
        );
        // A refusing `sudo` first on `PATH`, never the host's: a run that
        // isn't root relaunches through sudo (`logs` needs root), so a test
        // that runs one without the root gate fails, saying why.
        sb_cli_core::testing::write_program(
            &bin.join("sudo"),
            "#!/bin/sh\necho 'sb logs tests: sudo refused (a test without root_gate::requested?)' >&2\nexit 97\n",
        );
        std::fs::create_dir(dir.path().join("journal")).expect("journal");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        Self {
            dir,
            docker_host: None,
            follow: OnceLock::new(),
        }
    }

    /// A service's log in this world is followed: a follow slot, held
    /// while the world lives, after a check that the host has an inotify
    /// instance to give (sb-journal's `testing::follow_slot`; the nextest
    /// `journal-follow` group under nextest).
    pub fn follows(&self) {
        self.follow.get_or_init(follow_slot::take);
    }

    /// The same world on the Docker daemon at `host` (a fake's).
    #[must_use]
    pub fn on_docker(mut self, host: String) -> Self {
        self.docker_host = Some(host);
        self
    }

    pub fn journal(&self) -> PathBuf {
        self.dir.path().join("journal")
    }

    /// The environment of every run: `common::apply_sb_env`'s, then the
    /// world's: the run log's and the journal's directories, the stand-ins
    /// (`systemctl`, the refusing `sudo`) first on `PATH`, and the daemon
    /// if one was given.
    pub fn envs(&self) -> Vec<(String, Option<String>)> {
        let mut extra = vec![
            (
                "SB_LOG_DIR",
                self.dir.path().join("logs").display().to_string(),
            ),
            ("SB_JOURNAL_DIR", self.journal().display().to_string()),
            (
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.path().join("bin").display()),
            ),
        ];
        if let Some(host) = &self.docker_host {
            extra.push(("DOCKER_HOST", host.clone()));
        }
        sb_envs(&extra)
    }

    /// sb with `args`, piped.
    pub fn piped(&self, args: &[&str]) -> Output {
        self.piped_cmd(common::sb(), args)
    }

    /// sb with `args`, piped, in a session of its own (util-linux's
    /// `setsid`, waiting for it): no controlling terminal, so no
    /// `/dev/tty` either, whatever the test runs under.
    pub fn without_terminal(&self, args: &[&str]) -> Output {
        let mut cmd = std::process::Command::new("setsid");
        cmd.arg("--wait").arg(common::sb().get_program());
        cmd.stdin(std::process::Stdio::null());
        self.piped_cmd(cmd, args)
    }

    fn piped_cmd(&self, mut cmd: std::process::Command, args: &[&str]) -> Output {
        cmd.args(args).env("NO_COLOR", "1");
        for (k, v) in self.envs() {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        cmd.output().expect("run sb")
    }

    /// The run log's text.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("logs/sb.log")).unwrap_or_default()
    }
}

/// `common::apply_sb_env`'s environment, then `NO_COLOR=1` (which that
/// environment removes, and these runs keep unless a test removes it),
/// then `extra`'s values.
pub fn sb_envs(extra: &[(&str, String)]) -> Vec<(String, Option<String>)> {
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
    envs.push(("NO_COLOR".to_owned(), Some("1".to_owned())));
    for (k, v) in extra {
        envs.push(((*k).to_owned(), Some(v.clone())));
    }
    envs
}

/// sb on a PTY, its screen parsed as it arrives.
pub struct Pty {
    child: Box<dyn Child + Send + Sync>,
    // Both ends kept open, as a shell keeps its terminal: the termios
    // check reads the terminal after sb is gone.
    _master: Box<dyn MasterPty + Send>,
    _slave: Box<dyn SlavePty + Send>,
    tty: PathBuf,
    writer: Box<dyn Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    pub parser: vt100::Parser,
}

impl Pty {
    /// sb with `TERM=xterm-256color` and `NO_COLOR`, then `args` and
    /// `envs` ([`sb_envs`]; one with no value removes it).
    pub fn spawn(args: &[&str], envs: Vec<(String, Option<String>)>) -> Self {
        Self::spawn_program(common::sb().get_program(), args, envs)
    }

    /// `/bin/sh -c script sb args...`, as [`Pty::spawn`]: sb is `$0` and
    /// `args` follow, so `script` can redirect sb's output (its stdout a
    /// pipe or a file while the PTY stays its terminal).
    pub fn spawn_shell(script: &str, args: &[&str], envs: Vec<(String, Option<String>)>) -> Self {
        let sb = common::sb().get_program().to_string_lossy().into_owned();
        let mut all = vec!["-c", script, sb.as_str()];
        all.extend_from_slice(args);
        Self::spawn_program("/bin/sh".as_ref(), &all, envs)
    }

    fn spawn_program(
        program: &std::ffi::OsStr,
        args: &[&str],
        envs: Vec<(String, Option<String>)>,
    ) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.env("TERM", "xterm-256color");
        cmd.env("NO_COLOR", "1");
        for (k, v) in envs {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
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

    /// sb's soft and hard `RLIMIT_NOFILE` while it runs, `None` for
    /// unlimited, from its `/proc/<pid>/limits` row "Max open files"
    /// (proc(5): the soft limit, the hard limit, the unit). Why not
    /// procfs's `Process::limits`, a workspace dependency: in sb's
    /// dev-dependencies it changes `Cargo.lock`, which makes a task a
    /// shared-files merge (module plan, rule 3) for this test helper.
    pub fn open_files_limit(&self) -> (Option<u64>, Option<u64>) {
        let pid = self.child.process_id().expect("a pid");
        let limits = std::fs::read_to_string(format!("/proc/{pid}/limits")).expect("limits");
        let row = limits
            .lines()
            .find_map(|l| l.strip_prefix("Max open files"))
            .expect("the open files row");
        let mut fields = row.split_whitespace().map(|f| f.parse::<u64>().ok());
        (
            fields.next().expect("the soft limit"),
            fields.next().expect("the hard limit"),
        )
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write");
        self.writer.flush().expect("flush");
    }

    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }

    /// Wait until the screen shows `text`; false at the timeout.
    pub fn showing(&mut self, text: &str) -> bool {
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
    pub fn exit(&mut self) -> ExitStatus {
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
    pub fn assert_restored(&self) {
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

/// The soft `RLIMIT_NOFILE` the logs commands raise to under `hard`
/// (`None` for unlimited): min(hard, systemd's 262144), as journalctl's
/// (tui spec, "`RLIMIT_NOFILE`").
pub fn raised_open_files(hard: Option<u64>) -> u64 {
    hard.map_or(262_144, |hard| hard.min(262_144))
}

/// `path`'s text once it holds `text`; fails at the timeout.
pub fn file_holding(path: &std::path::Path, text: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let held = std::fs::read_to_string(path).unwrap_or_default();
        if held.contains(text) {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "no {text:?} in {}: {held:?}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
