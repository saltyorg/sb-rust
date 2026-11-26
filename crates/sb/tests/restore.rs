//! `sb restore` end to end (ops spec, Testing Strategy, "restore, end to
//! end" and "restore, prompts"): the built sb as root (`SB_TEST_ROOT=1`),
//! its Saltbox root and run log in the test's directory, against sb-ops'
//! fake restore service through `SB_RESTORE_URL`; the ciphertexts are the
//! openssl-made fixtures (crates/ops/tests/fixtures/restore). No real
//! service is reached.

mod common;
mod root_gate;

use std::io::{Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};
use sb_ops::restore::user_key;
use sb_ops::testing::restore::{Reply, RestoreService};

/// The fixtures' user (any name) and password (made-with.sh's).
const USER: &str = "synthetic-restore-user";
const PASSWORD: &str = "synthetic-restore-pass";

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/../ops/tests/fixtures/restore/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|err| panic!("{path}: {err}"))
}

/// A test's world: the fake service, a Saltbox root and a log directory.
struct World {
    dir: tempfile::TempDir,
    service: RestoreService,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::create_dir(dir.path().join("logs")).expect("logs");
        Self {
            dir,
            service: RestoreService::start().expect("fake"),
        }
    }

    /// `file`'s path on the service for [`USER`].
    fn path(file: &str) -> String {
        format!("/load/{}/{file}", user_key(USER).as_str())
    }

    fn serve(&self, file: &str, reply: Reply) {
        self.service.reply(&Self::path(file), reply);
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("saltbox")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("logs/sb.log")).unwrap_or_default()
    }

    fn envs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("SB_SALTBOX_DIR", self.root().display().to_string()),
            (
                "SB_LOG_DIR",
                self.dir.path().join("logs").display().to_string(),
            ),
            ("SB_RESTORE_URL", self.service.url().to_owned()),
        ]
    }

    /// `sb restore <args>` with no controlling terminal (`setsid -w`),
    /// `stdin` on its standard input.
    fn run(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut cmd = Command::new("setsid");
        common::apply_sb_env(&mut cmd);
        for (k, v) in self.envs() {
            cmd.env(k, v);
        }
        // No developer proxy between sb and the loopback fake.
        for var in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            cmd.env_remove(var);
        }
        cmd.arg("-w")
            .arg(assert_cmd::cargo::cargo_bin("sb"))
            .arg("restore")
            .args(args)
            .arg("--color=never")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("run sb");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin)
            .expect("write stdin");
        child.wait_with_output().expect("sb's output")
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

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).expect("metadata").mode() & 0o7777
}

/// Every outcome in one run: two files restored, a 404 and a redirect
/// ignored (the redirect not followed), and failures each as sb-go shows
/// them: no header, a body over 16 MiB, a dropped connection, a file
/// that isn't text (the wrong-password line).
#[test]
fn every_file_shows_its_outcome() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    // A header, a salt and 17 bytes: not whole blocks.
    world.serve(
        "accounts.yml",
        Reply::Body([&b"Salted__12345678"[..], &[0u8; 17]].concat()),
    );
    world.serve("adv_settings.yml", Reply::Body(fixture("utf8.yml.enc")));
    world.serve("backup_config.yml", Reply::Body(fixture("long.conf.enc")));
    world.serve("hetzner_vlan.yml", Reply::Body(fixture("binary.dat.enc")));
    world.serve(
        "localhost.yml",
        Reply::Cut {
            sent: b"Salted__".to_vec(),
            len: 4096,
        },
    );
    world.serve("motd.yml", Reply::Body(b"not encrypted".to_vec()));
    world.serve(
        "providers.yml",
        Reply::Redirect(format!("{}/elsewhere", world.service.url())),
    );
    world.serve("rclone.conf", Reply::Body(vec![b'x'; (16 << 20) + 1]));
    // settings.yml unscripted: the service's 404.
    let out = world.run(
        &["--user", USER, "--password-file", "-"],
        format!("{PASSWORD}\n").as_bytes(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "Fetching files from {}...\n\
             accounts.yml         [FAIL]\n  \
             (Technical error: decryption failed: invalid ciphertext length: 17)\n\
             adv_settings.yml     [DONE]\n\
             backup_config.yml    [DONE]\n\
             hetzner_vlan.yml     [FAIL]\n\
             Decryption failed. This likely means the password was incorrect\n\
             localhost.yml        [FAIL]\n\
             motd.yml             [FAIL]\n\
             providers.yml        [IGNORED]\n\
             rclone.conf          [FAIL]\n\
             settings.yml         [IGNORED]\n\
             Restore process completed: 2 files successfully restored.\n",
            world.service.url()
        )
    );
    let root = world.root();
    assert_eq!(
        std::fs::read(root.join("adv_settings.yml")).expect("restored"),
        fixture("utf8.yml")
    );
    assert_eq!(
        std::fs::read(root.join("backup_config.yml")).expect("restored"),
        fixture("long.conf")
    );
    for file in ["adv_settings.yml", "backup_config.yml"] {
        assert_eq!(mode(&root.join(file)), 0o600, "{file}");
    }
    assert_eq!(mode(&root), 0o755, "the root, created");
    for file in [
        "hetzner_vlan.yml",
        "localhost.yml",
        "motd.yml",
        "rclone.conf",
        "accounts.yml",
    ] {
        assert!(!root.join(file).exists(), "{file} placed");
    }
    let log = world.log();
    assert!(
        log.contains("the file is over 16777216 bytes"),
        "the cap named for rclone.conf: {log}"
    );
    assert_eq!(
        log.matches("the file is over").count(),
        1,
        "only the oversized body: {log}"
    );
    let seen = world.service.seen();
    assert_eq!(
        seen.len(),
        9,
        "one request a file, the redirect not followed: {seen:?}"
    );
    assert!(
        seen.iter().all(|s| s
            .user_agent
            .as_deref()
            .is_some_and(|ua| ua.starts_with("sb/"))),
        "{seen:?}"
    );
}

/// A file that can't be placed (a directory at its name) fails with
/// sb-go's line; the others go on.
#[test]
fn a_file_that_cant_be_placed_fails_with_its_cause() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.serve("settings.yml", Reply::Body(fixture("settings.yml.enc")));
    world.serve("rclone.conf", Reply::Body(fixture("long.conf.enc")));
    std::fs::create_dir_all(world.root().join("settings.yml")).expect("a directory there");
    let out = world.run(
        &["--user", USER, "--password-file", "-"],
        format!("{PASSWORD}\n").as_bytes(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    assert!(
        stdout(&out).contains(
            "settings.yml         [FAIL]\n  (Technical error: failed to move file: Is a directory (os error 21))\n"
        ),
        "{}",
        both(&out)
    );
    assert!(
        stdout(&out).contains("rclone.conf          [DONE]"),
        "{}",
        both(&out)
    );
}

/// A first signal while a file is fetched: the loop stops there, nothing
/// more is asked, sb ends by the signal, and no temporary file is left.
#[test]
fn a_first_signal_mid_fetch_stops_the_restore() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.serve("adv_settings.yml", Reply::Stall);
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("sb"));
    common::apply_sb_env(&mut cmd);
    for (k, v) in world.envs() {
        cmd.env(k, v);
    }
    let mut child = cmd
        .args([
            "restore",
            "--user",
            USER,
            "--password-file",
            "-",
            "--color=never",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run sb");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(format!("{PASSWORD}\n").as_bytes())
        .expect("stdin");
    let deadline = Instant::now() + WAIT;
    while world.service.seen().len() < 2 {
        assert!(Instant::now() < deadline, "the stalled request never came");
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).expect("pid")).expect("pid");
    rustix::process::kill_process(pid, rustix::process::Signal::INT).expect("SIGINT");
    let out = child.wait_with_output().expect("sb's output");
    assert_eq!(out.status.signal(), Some(2), "{}", both(&out));
    assert_eq!(
        world.service.seen().len(),
        2,
        "nothing asked after the signal"
    );
    let left: Vec<_> = std::fs::read_dir(world.root())
        .map_or_default(|dir| dir.filter_map(Result::ok).map(|e| e.file_name()).collect());
    assert_eq!(
        left,
        Vec::<std::ffi::OsString>::new(),
        "no file, temporary or not"
    );
    assert!(stderr(&out).contains("interrupted"), "{}", both(&out));
}

/// Nothing listening at the service: each file fails, the run fails, and
/// the log has the cause, never the key.
#[test]
fn a_service_that_cant_be_reached_fails_with_its_cause_in_the_log() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut envs = world.envs();
    // Port 0 can't be connected to: refused at once, no other process
    // can be listening there (unlike a port freed just before).
    envs.retain(|(k, _)| *k != "SB_RESTORE_URL");
    let password_file = world.dir.path().join("password");
    std::fs::write(&password_file, PASSWORD).expect("password");
    let mut cmd = Command::new("setsid");
    common::apply_sb_env(&mut cmd);
    for (k, v) in &envs {
        cmd.env(k, v);
    }
    let out = cmd
        .env("SB_RESTORE_URL", "http://localhost:0")
        .arg("-w")
        .arg(assert_cmd::cargo::cargo_bin("sb"))
        .args(["restore", "--user", USER, "--password-file"])
        .arg(&password_file)
        .arg("--color=never")
        .output()
        .expect("run sb");
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        stdout(&out).contains("accounts.yml         [FAIL]\n"),
        "{}",
        both(&out)
    );
    let log = world.log();
    assert!(log.contains("restore fetch failed"), "{log}");
    assert!(
        log.contains("error sending request: "),
        "a cause after reqwest's kind: {log}"
    );
    let key = user_key(USER);
    for secret in [USER, key.as_str(), PASSWORD] {
        assert!(!log.contains(secret), "{secret:?} in the log:\n{log}");
    }
}

/// Nothing restored: sb-go's error, exit 1.
#[test]
fn nothing_restored_fails() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.serve("settings.yml", Reply::Body(fixture("settings.yml.enc")));
    let out = world.run(&["--user", USER, "--password-file", "-"], b"wrong\n");
    assert_eq!(out.status.code(), Some(1), "{}", both(&out));
    assert!(
        stdout(&out).contains("settings.yml         [FAIL]\nDecryption failed."),
        "{}",
        both(&out)
    );
    assert!(
        stderr(&out).contains("restore process failed: no files were downloaded or decrypted"),
        "{}",
        both(&out)
    );
    assert!(!world.root().join("settings.yml").exists());
}

/// The user name, its key and the password reach no output and no run
/// log (ops decision 10); the URLs logged carry `[REDACTED]`.
#[test]
fn no_secret_reaches_the_output_or_the_log() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.serve("settings.yml", Reply::Body(fixture("settings.yml.enc")));
    world.serve(
        "rclone.conf",
        Reply::Cut {
            sent: b"Salted__".to_vec(),
            len: 64,
        },
    );
    let password_file = world.dir.path().join("password");
    std::fs::write(&password_file, PASSWORD).expect("password file");
    let out = world.run(
        &[
            "--user",
            USER,
            "--password-file",
            &password_file.display().to_string(),
        ],
        b"",
    );
    assert_eq!(out.status.code(), Some(0), "{}", both(&out));
    let log = world.log();
    // A 404's URL is logged, its key redacted.
    assert!(log.contains("/load/[REDACTED]/accounts.yml"), "{log}");
    let key = user_key(USER);
    for secret in [USER, key.as_str(), PASSWORD] {
        for (name, text) in [
            ("stdout", stdout(&out)),
            ("stderr", stderr(&out)),
            ("log", log.clone()),
        ] {
            assert!(!text.contains(secret), "{secret:?} in {name}:\n{text}");
        }
    }
}

/// The password file: a path, `-` for stdin, an empty one refused; an
/// empty `--user` refused; with no terminal and no answer, the error
/// names the flag. Nothing is fetched when an answer is missing.
#[test]
fn the_answers_come_from_the_flags() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let empty = world.dir.path().join("empty");
    std::fs::write(&empty, "\n").expect("empty");
    let started = Instant::now();
    for (args, stdin, error) in [
        (
            vec!["--user", USER, "--password-file", "-"],
            &b""[..],
            "the password file is empty",
        ),
        (
            vec![
                "--user",
                USER,
                "--password-file",
                empty.to_str().expect("path"),
            ],
            b"",
            "the password file is empty",
        ),
        (
            vec!["--user", "", "--password-file", "-"],
            b"x\n",
            "the user name is empty",
        ),
        (
            vec!["--password-file", "-"],
            b"x\n",
            "answer it with --user",
        ),
        (vec!["--user", USER], b"", "answer it with --password-file"),
    ] {
        let out = world.run(&args, stdin);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {}", both(&out));
        assert!(stderr(&out).contains(error), "{args:?}: {}", both(&out));
        assert_eq!(stdout(&out), "", "{args:?}");
    }
    assert_eq!(world.service.seen(), Vec::new(), "fetched without answers");
    // Never a hang: five refusals well within the time a prompt would wait.
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

/// The help: sb-go's texts, the two flags, no `-v`.
#[test]
fn the_help_has_the_answer_flags_and_no_verbose() {
    let out = common::sb()
        .args(["restore", "--help", "--color=never"])
        .output()
        .expect("run sb");
    let help = stdout(&out);
    for text in [
        "Fetches encrypted files from a remote URL",
        "--user",
        "Restore service user name (asked on a terminal when not given)",
        "--password-file",
        "Read the password from this file, or stdin with '-' (asked on a terminal when not given)",
    ] {
        assert!(help.contains(text), "{text}: {help}");
    }
    assert!(!help.contains("--verbose"), "{help}");
}

// ---- on a terminal ----------------------------------------------------

/// How long a PTY test waits for the screen or for sb to exit.
const WAIT: Duration = Duration::from_secs(20);

/// sb on a PTY of 100×30, its screen parsed through vt100: the log
/// screens' harness, cut to what these tests use. Why not that module:
/// most of it would be dead code here, which the gate's warnings refuse.
struct Pty {
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    tty: PathBuf,
    writer: Box<dyn std::io::Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    parser: vt100::Parser,
}

impl Pty {
    fn spawn(args: &[&str], envs: &[(&str, String)]) -> Self {
        Self::spawn_with(None, args, envs)
    }

    /// sb with `stdin` piped in by a shell (`printf | exec sb`), stdout
    /// and the keys still the terminal's.
    fn spawn_piped(stdin: &str, args: &[&str], envs: &[(&str, String)]) -> Self {
        Self::spawn_with(Some(stdin), args, envs)
    }

    fn spawn_with(stdin: Option<&str>, args: &[&str], envs: &[(&str, String)]) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        let mut base = common::sb();
        base.args(args);
        // With `stdin`: sh -c 's=$1; shift; printf ... | exec "$0" "$@"',
        // sb as $0.
        let mut cmd = if let Some(stdin) = stdin {
            let mut shell = CommandBuilder::new("sh");
            shell.args([
                "-c",
                "s=$1; shift; printf '%s\\n' \"$s\" | exec \"$0\" \"$@\"",
            ]);
            shell.arg(base.get_program());
            shell.arg(stdin);
            shell.args(base.get_args());
            shell
        } else {
            let mut sb = CommandBuilder::new(base.get_program());
            sb.args(base.get_args());
            sb
        };
        // In the test's working directory, where a std `Command` starts its
        // child: portable-pty starts it in `$HOME` unless given one
        // (`CommandBuilder::cwd`, the fallback in `as_command`:
        // <https://docs.rs/crate/portable-pty/0.9.0/source/src/cmdbuilder.rs>).
        cmd.cwd(std::env::current_dir().expect("cwd"));
        for (k, v) in base.get_envs() {
            match v {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            }
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("NO_COLOR", "1");
        for var in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            cmd.env_remove(var);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn");
        drop(pair.slave);
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

    /// The terminal echoes again and reads lines: the screen's raw mode
    /// is gone.
    fn assert_restored(&self) {
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

/// On a terminal, the form asks: the user name shown as typed, the
/// passwords masked, a mismatch shown in the form with sb-go's text; then
/// the run, the viewer on the restored file with the check line, and
/// after Continue the final list on the terminal in the line form. Nothing secret stays on
/// the terminal and it echoes again.
#[test]
fn on_a_terminal_the_form_asks_and_the_list_shows() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.serve("settings.yml", Reply::Body(fixture("settings.yml.enc")));
    let mut pty = Pty::spawn(&["restore"], &world.envs());
    assert!(pty.showing("Repeat password"), "{}", pty.contents());
    pty.send(format!("{USER}\tfirst-try\tsecond-try\r").as_bytes());
    assert!(pty.showing(USER), "{}", pty.contents());
    assert!(
        pty.showing("Error: passwords do not match or are empty"),
        "{}",
        pty.contents()
    );
    assert!(!pty.contents().contains("first-try"), "{}", pty.contents());
    // The repeat cleared and typed again, then the password (Shift-Tab).
    pty.send(&[0x7f; 10]);
    pty.send(PASSWORD.as_bytes());
    pty.send(b"\x1b[Z");
    pty.send(&[0x7f; 9]);
    pty.send(format!("{PASSWORD}\r\r").as_bytes());
    assert!(
        pty.showing("Check each restored file"),
        "{}",
        pty.contents()
    );
    assert!(pty.showing("settings.yml read-only"), "{}", pty.contents());
    assert!(pty.showing("synthetic: true"), "{}", pty.contents());
    for drawn in [
        "settings.yml      DONE",
        "accounts.yml      IGNORED",
        "[ Continue ]",
    ] {
        assert!(pty.showing(drawn), "{drawn}:\n{}", pty.contents());
    }
    // Tab to the content, then to Continue, and Enter on it.
    pty.send(b"\t\t\r");
    let status = pty.exit();
    assert!(status.success(), "{status:?}\n{}", pty.contents());
    let screen = pty.contents();
    assert!(
        screen.contains("Fetching files from http://"),
        "the final list:\n{screen}"
    );
    assert!(screen.contains("settings.yml         [DONE]"), "{screen}");
    assert!(
        screen.contains("accounts.yml         [IGNORED]"),
        "{screen}"
    );
    assert!(
        screen.contains("Restore process completed: 1 files successfully restored."),
        "{screen}"
    );
    let log = world.log();
    assert!(log.contains("restore screen closed"), "{log}");
    let key = user_key(USER);
    for secret in [USER, PASSWORD, "first-try", "second-try", key.as_str()] {
        assert!(!screen.contains(secret), "{secret:?} left:\n{screen}");
        assert!(!log.contains(secret), "{secret:?} logged:\n{log}");
    }
    // The restored file showed in the viewer, as it is; its content never
    // reaches the run log (ops spec, "No masking on screen").
    assert!(!log.contains("synthetic: true"), "{log}");
    pty.assert_restored();
    assert_eq!(
        std::fs::read(world.root().join("settings.yml")).expect("restored"),
        fixture("settings.yml")
    );
}

/// Ctrl-C in the form ends sb with 130, quietly, the terminal restored
/// and nothing fetched.
#[test]
fn ctrl_c_in_the_form_exits_130() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&["restore", "--user", USER], &world.envs());
    assert!(pty.showing("Repeat password"), "{}", pty.contents());
    pty.send(b"half");
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{status:?}\n{}", pty.contents());
    pty.assert_restored();
    assert_eq!(world.service.seen(), Vec::new());
}

/// Esc in the form is sb-go's cancel: `Restore cancelled.`, exit 0.
#[test]
fn esc_in_the_form_cancels() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    let mut pty = Pty::spawn(&["restore"], &world.envs());
    assert!(pty.showing("Repeat password"), "{}", pty.contents());
    pty.send(b"\x1b");
    let status = pty.exit();
    assert!(status.success(), "{status:?}\n{}", pty.contents());
    assert!(
        pty.contents().contains("Restore cancelled."),
        "{}",
        pty.contents()
    );
    pty.assert_restored();
    assert_eq!(world.service.seen(), Vec::new());
}

/// Ctrl-C on the list mid-run ends sb with 130, the files listed so far
/// printed, nothing half-placed.
#[test]
fn ctrl_c_mid_run_exits_130_with_the_list_so_far() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.serve("accounts.yml", Reply::Body(fixture("settings.yml.enc")));
    world.serve("adv_settings.yml", Reply::Stall);
    let mut pty = Pty::spawn(&["restore", "--user", USER], &world.envs());
    assert!(pty.showing("Repeat password"), "{}", pty.contents());
    pty.send(format!("{PASSWORD}\t{PASSWORD}\r").as_bytes());
    assert!(
        pty.showing("adv_settings.yml     fetching"),
        "{}",
        pty.contents()
    );
    pty.send(b"\x03");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 130, "{status:?}\n{}", pty.contents());
    let screen = pty.contents();
    assert!(screen.contains("accounts.yml         [DONE]"), "{screen}");
    assert!(!screen.contains("adv_settings.yml"), "{screen}");
    pty.assert_restored();
    let placed: Vec<_> = std::fs::read_dir(world.root())
        .expect("root")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(placed, ["accounts.yml"]);
}

/// `--password-file -` with a terminal on stdin leaves the password to
/// the form instead of reading the terminal with echo on.
#[test]
fn password_file_dash_on_a_terminal_asks() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.serve("settings.yml", Reply::Body(fixture("settings.yml.enc")));
    let mut pty = Pty::spawn(
        &["restore", "--user", USER, "--password-file", "-"],
        &world.envs(),
    );
    assert!(pty.showing("Repeat password"), "{}", pty.contents());
    pty.send(format!("{PASSWORD}\t{PASSWORD}\r").as_bytes());
    assert!(
        pty.showing("Check each restored file"),
        "{}",
        pty.contents()
    );
    pty.send(b"q");
    assert!(pty.exit().success(), "{}", pty.contents());
    assert!(
        pty.contents()
            .contains("Restore process completed: 1 files successfully restored."),
        "{}",
        pty.contents()
    );
    assert!(!pty.contents().contains(PASSWORD), "{}", pty.contents());
    pty.assert_restored();
}

/// Both answers given (a password piped in with `--password-file -`)
/// with stdout a terminal: no screen, sb-go's lines (decision 32).
#[test]
fn both_flags_on_a_terminal_give_the_line_form() {
    if !root_gate::requested() {
        return;
    }
    let world = World::new();
    world.serve("settings.yml", Reply::Body(fixture("settings.yml.enc")));
    let mut pty = Pty::spawn_piped(
        PASSWORD,
        &["restore", "--user", USER, "--password-file", "-"],
        &world.envs(),
    );
    assert!(pty.exit().success(), "{}", pty.contents());
    let screen = pty.contents();
    assert!(screen.contains("Fetching files from http://"), "{screen}");
    assert!(screen.contains("settings.yml         [DONE]"), "{screen}");
    assert!(
        screen.contains("Restore process completed: 1 files successfully restored."),
        "{screen}"
    );
    assert!(!screen.contains("Check each restored file"), "{screen}");
    pty.assert_restored();
}
