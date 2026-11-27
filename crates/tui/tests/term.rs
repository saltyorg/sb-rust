//! The screen on a real PTY (tui spec, Testing Strategy): setup, every way
//! out restoring the terminal (alternate screen left, mouse and bracketed
//! paste off, raw mode
//! off read back from the PTY's termios), and the input latency of the
//! event loop on the current-thread runtime.
//!
//! Each test re-runs this test binary as the child on the PTY (filtered to
//! `pty_child`, with `SB_TUI_CHILD` set), as cli-core's signal tests do,
//! and reads its screen through vt100. The `/dev/tty` screen of the inline
//! fallback (`Tui::on_tty`) runs the same ways out with the child's stdout
//! a file, which must get none of the screen's codes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode};
use portable_pty::{
    Child, CommandBuilder, ExitStatus, MasterPty, PtySize, SlavePty, native_pty_system,
};
use ratatui::Frame;
use ratatui::widgets::Paragraph;
use rustix::fs::{Mode, OFlags, open};
use rustix::process::{Pid, Signal, kill_process};
use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::exit::finish_run;
use sb_cli_core::log::RunLog;
use sb_tui::{App, Flow, Tui};

/// The child's mode: `screen` (the screen below), `dropped` (the same,
/// dropped instead of left) or `baseline` (raw mode and a plain read
/// loop, the harness's own latency).
const CHILD: &str = "SB_TUI_CHILD";
/// The run log's directory in the child.
const LOG_DIR: &str = "SB_TUI_LOG";
/// Set: the child's screen is on `/dev/tty` (`Tui::on_tty`).
const ON_TTY: &str = "SB_TUI_ON_TTY";
const WAIT: Duration = Duration::from_secs(20);
/// Longer than cli-core's 250 ms repeat window: a second SIGTERM this long
/// after the first is a second signal, not a copy.
const PAST_THE_WINDOW: Duration = Duration::from_millis(400);

/// Counts keys and shows the count; `q` quits, `p` panics, `t` panics on
/// another thread and exits, `s` blocks the runtime's thread (a stuck
/// screen, for the forced exit).
struct Keys(usize);

impl App for Keys {
    fn input(&mut self, input: Event) -> Flow {
        let Event::Key(key) = input else {
            return Flow::Unchanged;
        };
        match key.code {
            KeyCode::Char('q') => Flow::Quit,
            KeyCode::Char('p') => panic!("a planned panic"),
            // A panic on another thread, then an exit that drops nothing:
            // only the panic hooks restore the terminal.
            KeyCode::Char('t') => {
                let _ = std::thread::spawn(|| panic!("a thread's panic")).join();
                std::process::exit(3)
            }
            KeyCode::Char('s') => {
                let mut out = std::io::stdout();
                write!(out, "stuck").expect("write");
                out.flush().expect("flush");
                std::thread::sleep(Duration::from_secs(60));
                Flow::Unchanged
            }
            _ => {
                self.0 += 1;
                Flow::Redraw
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        frame.render_widget(Paragraph::new(format!("keys {}", self.0)), frame.area());
    }
}

/// The child: a run log with cli-core's panic record, the signals thread,
/// then the screen on a current-thread runtime, as sb runs.
#[test]
fn pty_child() {
    let Ok(mode) = std::env::var(CHILD) else {
        return;
    };
    if mode == "baseline" {
        baseline();
        return;
    }
    let dir = std::env::var_os(LOG_DIR).expect("log dir");
    let (log, subscriber) = RunLog::open(Path::new(&dir)).expect("run log");
    tracing::subscriber::set_global_default(subscriber).expect("subscriber");
    log.record_panics();
    let shutdown = Shutdown::new();
    shutdown.listen_for_signals().expect("signals");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let on_tty = std::env::var_os(ON_TTY).is_some();
    let ending = runtime.block_on(async {
        let mut tui = if on_tty {
            let tty = std::fs::File::options()
                .read(true)
                .write(true)
                .open("/dev/tty")?;
            Tui::on_tty(&shutdown, tty)?
        } else {
            Tui::enter(&shutdown)?
        };
        let ending = tui.run(&mut Keys(0)).await;
        // `dropped`: the screen goes without `leave`, as on an early `?`.
        if mode == "dropped" {
            drop(tui);
        } else {
            tui.leave().await?;
        }
        ending
    });
    println!("ending {ending:?}");
    // As sb's main ends a run: by the signal after a SIGTERM.
    let _ = std::process::Termination::report(finish_run(
        &shutdown.snapshot(),
        None,
        None,
        &mut std::io::stderr(),
    ));
}

/// The harness's baseline: raw mode, then each byte read shows the count
/// at once.
fn baseline() {
    crossterm::terminal::enable_raw_mode().expect("raw mode");
    let mut out = std::io::stdout();
    let mut count = 0;
    write!(out, "\r\nkeys {count}").expect("write");
    out.flush().expect("flush");
    let mut byte = [0];
    while std::io::stdin().read(&mut byte).expect("read") == 1 && byte[0] != b'q' {
        count += 1;
        write!(out, "\r\nkeys {count}").expect("write");
        out.flush().expect("flush");
    }
    crossterm::terminal::disable_raw_mode().expect("cooked mode");
}

/// The child on a PTY, its screen parsed as it arrives.
struct Pty {
    child: Box<dyn Child + Send + Sync>,
    // Both ends kept open, as a shell keeps its terminal: the termios
    // check reads the terminal after the child is gone, and input that
    // comes after the restore is echoed, as a shell's terminal echoes it.
    _master: Box<dyn MasterPty + Send>,
    _slave: Box<dyn SlavePty + Send>,
    tty: PathBuf,
    writer: Box<dyn Write + Send>,
    output: mpsc::Receiver<Vec<u8>>,
    parser: vt100::Parser,
    /// Every byte the child wrote.
    raw: Vec<u8>,
    log: tempfile::TempDir,
}

impl Pty {
    fn spawn(mode: &str) -> Self {
        Self::spawn_with(mode, false)
    }

    /// The child with its screen on `/dev/tty`, its stdout the file
    /// [`Pty::stdout`] reads (through `/bin/sh`, which then `exec`s it).
    fn spawn_on_tty(mode: &str) -> Self {
        Self::spawn_with(mode, true)
    }

    fn spawn_with(mode: &str, on_tty: bool) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        let log = tempfile::tempdir().expect("log dir");
        let exe = std::env::current_exe().expect("test binary");
        let child_args = ["pty_child", "--exact", "--nocapture", "--test-threads=1"];
        let mut cmd = if on_tty {
            let mut cmd = CommandBuilder::new("/bin/sh");
            let script = r#"exec "$0" "$@" > "$STDOUT""#;
            cmd.arg("-c");
            cmd.arg(script);
            cmd.arg(&exe);
            cmd.args(child_args);
            cmd.env("STDOUT", log.path().join("stdout"));
            cmd.env(ON_TTY, "1");
            cmd
        } else {
            let mut cmd = CommandBuilder::new(&exe);
            cmd.args(child_args);
            cmd
        };
        cmd.cwd(std::env::current_dir().expect("cwd"));
        cmd.env(CHILD, mode);
        cmd.env(LOG_DIR, log.path().join("log"));
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
            parser: vt100::Parser::new(24, 80, 0),
            raw: Vec::new(),
            log,
        }
    }

    fn process(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.raw.extend_from_slice(bytes);
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write");
        self.writer.flush().expect("flush");
    }

    fn signal(&self, signal: Signal) {
        let pid = self.child.process_id().expect("pid");
        let pid = Pid::from_raw(i32::try_from(pid).expect("pid")).expect("pid");
        kill_process(pid, signal).expect("kill");
    }

    /// Wait until `done` holds for the screen; false at the timeout.
    fn wait_for(&mut self, timeout: Duration, done: impl Fn(&vt100::Screen) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if done(self.parser.screen()) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.output.recv_timeout(left) {
                Ok(bytes) => self.process(&bytes),
                Err(mpsc::RecvTimeoutError::Timeout) => return false,
                Err(mpsc::RecvTimeoutError::Disconnected) => return done(self.parser.screen()),
            }
        }
    }

    /// Wait until the child has written `bytes`; false at the timeout.
    fn wrote(&mut self, bytes: &[u8]) -> bool {
        let deadline = Instant::now() + WAIT;
        while !self.raw.windows(bytes.len()).any(|w| w == bytes) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.output.recv_timeout(left) {
                Ok(bytes) => self.process(&bytes),
                Err(_) => return false,
            }
        }
        true
    }

    /// Whether the child has written `bytes` by now, reading what has
    /// already arrived without waiting.
    fn wrote_now(&mut self, bytes: &[u8]) -> bool {
        while let Ok(more) = self.output.try_recv() {
            self.process(&more);
        }
        self.raw.windows(bytes.len()).any(|w| w == bytes)
    }

    fn showing(&mut self, text: &str) -> bool {
        self.wait_for(WAIT, |screen| screen.contents().contains(text))
    }

    /// Wait for the child to exit, reading its output meanwhile.
    fn exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                // What it wrote before exiting.
                while let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(200)) {
                    self.process(&bytes);
                }
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the child didn't exit:\n{}",
                self.parser.screen().contents()
            );
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
                self.process(&bytes);
            }
        }
    }

    /// The screen is up: the alternate screen, mouse capture and bracketed
    /// paste on.
    fn assert_up(&mut self) {
        assert!(
            self.showing("keys 0"),
            "{}",
            self.parser.screen().contents()
        );
        let screen = self.parser.screen();
        assert!(screen.alternate_screen());
        assert_ne!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
        // A paste comes as one event, not as keys.
        assert!(screen.bracketed_paste());
    }

    /// The terminal is restored: the alternate screen left, mouse capture
    /// and bracketed paste off, the cursor shown, and the PTY back in canonical mode with echo.
    fn assert_restored(&self) {
        let screen = self.parser.screen();
        assert!(!screen.alternate_screen(), "{}", screen.contents());
        assert_eq!(screen.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
        assert!(!screen.bracketed_paste(), "{}", screen.contents());
        assert!(!screen.hide_cursor());
        // NOCTTY: the test process never takes the PTY as its terminal.
        let tty = open(&self.tty, OFlags::RDONLY | OFlags::NOCTTY, Mode::empty()).expect("tty");
        let modes = tcgetattr(&tty).expect("termios").local_modes;
        assert!(
            modes.contains(LocalModes::ICANON | LocalModes::ECHO),
            "{modes:?}"
        );
    }

    /// Nothing the child left on the terminal is queued for the shell.
    ///
    /// Called once the child has exited: the input still on the terminal
    /// is what the shell would read (or echo) next, raw-mode bytes
    /// included; the bytes and the screen are shown if some are. The count
    /// is the kernel's (`FIONREAD`, rustix's `ioctl_fionread`:
    /// <https://man7.org/linux/man-pages/man2/ioctl_tty.2.html>,
    /// `TIOCINQ`), taken with canonical mode off a moment and then put
    /// back: in canonical mode it counts only up to the last line end
    /// (`n_tty`'s `inq_canon`, in `n_tty_ioctl`:
    /// <https://github.com/torvalds/linux/blob/v6.8/drivers/tty/n_tty.c>),
    /// so bytes that came after the restore without a newline would go
    /// uncounted; bytes left from raw mode become a line when canonical
    /// mode comes back (`n_tty_set_termios`). The switch drops nothing,
    /// `TCSANOW` flushes nothing, and what is counted is read here first.
    fn assert_nothing_queued(&self) {
        // NOCTTY: the test process never takes the PTY as its terminal;
        // NONBLOCK: the read of what is counted never waits.
        let tty = open(
            &self.tty,
            OFlags::RDONLY | OFlags::NOCTTY | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .expect("tty");
        let modes = tcgetattr(&tty).expect("termios");
        let mut counting = modes.clone();
        counting.local_modes.remove(LocalModes::ICANON);
        tcsetattr(&tty, OptionalActions::Now, &counting).expect("non-canonical");
        let queued = rustix::io::ioctl_fionread(&tty).expect("FIONREAD");
        let mut bytes = vec![0; usize::try_from(queued).expect("a count")];
        match rustix::io::read(&tty, &mut bytes) {
            Ok(read) => bytes.truncate(read),
            Err(e) => bytes = format!("<not read: {e}>").into_bytes(),
        }
        tcsetattr(&tty, OptionalActions::Now, &modes).expect("the modes back");
        assert_eq!(
            queued,
            0,
            "queued for the shell: {:?}\n{}",
            String::from_utf8_lossy(&bytes),
            self.parser.screen().contents()
        );
    }

    /// What the child wrote on its stdout when it ran on `/dev/tty`.
    fn stdout(&self) -> String {
        std::fs::read_to_string(self.log.path().join("stdout")).unwrap_or_default()
    }

    /// The `/dev/tty` screen wrote nothing of its own on stdout.
    fn assert_stdout_clean(&self) {
        let out = self.stdout();
        assert!(!out.contains('\x1b'), "{out:?}");
    }

    fn run_log(&self) -> String {
        std::fs::read_to_string(self.log.path().join("log/sb.log")).unwrap_or_default()
    }

    /// The `millis=` of the run log's last record holding `message`: how
    /// long the child itself measured (its own clock, not the test's).
    fn run_log_millis(&self, message: &str) -> u128 {
        let log = self.run_log();
        let record = log
            .lines()
            .rev()
            .find(|l| l.contains(message))
            .unwrap_or_else(|| panic!("no {message:?} in the run log:\n{log}"));
        record
            .split_whitespace()
            .find_map(|field| field.strip_prefix("millis="))
            .and_then(|millis| millis.parse().ok())
            .unwrap_or_else(|| panic!("no millis= in {record:?}"))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn q_leaves_the_screen_and_restores_the_terminal() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    pty.send(b"x");
    assert!(pty.showing("keys 1"));
    pty.send(b"q");
    let status = pty.exit();
    assert!(status.success(), "{status:?}");
    assert!(pty.parser.screen().contents().contains("ending Ok(Quit)"));
    pty.assert_restored();
    assert_cleared_before_leaving(&pty.raw);
}

/// An SGR mouse report, as a terminal sends one while capture is on.
const MOUSE_MOVE: &[u8] = b"\x1b[<35;10;5M";
/// The first code of crossterm's `DisableMouseCapture` (SGR reports off).
const MOUSE_OFF: &[u8] = b"\x1b[?1006l";

#[test]
fn q_leaves_the_screen_once_and_mouse_codes_after_it_never_reach_the_shell() {
    // A run where the harness was too slow to send the codes inside the
    // child's drain proves nothing either way; it is run again.
    for _ in 0..3 {
        let mut pty = Pty::spawn("screen");
        pty.assert_up();
        pty.send(b"q");
        // Mouse codes still on their way once capture is off: sent after
        // the child has turned it off, timed by the child's sequence
        // rather than the harness's clock.
        assert!(
            pty.wrote(MOUSE_OFF),
            "{:?}",
            String::from_utf8_lossy(&pty.raw)
        );
        for after in [0, 10, 20] {
            std::thread::sleep(Duration::from_millis(after));
            pty.send(MOUSE_MOVE);
        }
        // Already restored (raw mode goes off before the screen is left):
        // the codes may have come after the drain.
        let late = pty.wrote_now(b"\x1b[?1049l");
        assert!(pty.exit().success());
        pty.assert_restored();
        let raw = String::from_utf8_lossy(&pty.raw).into_owned();
        assert_eq!(raw.matches("\x1b[?1049l").count(), 1, "{raw:?}");
        if late {
            continue;
        }
        // Back in cooked mode the terminal would echo them (`^[[<35;10;5M`),
        // and the shell would read them.
        assert!(!raw.contains("<35;10;5M"), "{raw:?}");
        pty.assert_nothing_queued();
        return;
    }
    panic!("the harness never sent the codes inside the drain");
}

/// The terminal's answer to the exit drain's status query (DA1).
const ANSWER: &[u8] = b"\x1b[?62;22c";
/// The status query (DA1), as the exit drain writes it.
const QUERY: &[u8] = b"\x1b[c";

/// The exit drain ends at the terminal's answer: mouse codes sent before
/// it and the answer itself are read and dropped, never echoed to the
/// shell, and the exit doesn't wait the whole drain.
#[test]
fn the_exit_drain_ends_at_the_terminals_answer_and_nothing_reaches_the_shell() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    pty.send(b"q");
    assert!(pty.wrote(QUERY), "{:?}", String::from_utf8_lossy(&pty.raw));
    pty.send(MOUSE_MOVE);
    pty.send(ANSWER);
    assert!(pty.exit().success());
    pty.assert_restored();
    let raw = String::from_utf8_lossy(&pty.raw).into_owned();
    let after = &raw[raw.find("\x1b[c").expect("the query")..];
    assert!(!after.contains("<35;10;5M"), "{raw:?}");
    assert!(!after.contains("?62;22c"), "{raw:?}");
    pty.assert_nothing_queued();
    // Ended by the answer, not the 1 s cap (a wall-clock bound would fail
    // on a loaded host).
    assert!(pty.run_log().contains("answered=true"), "{}", pty.run_log());
}

/// A slow link, its answer 200 to 900 ms after the query: the drain
/// waits for it, mouse codes before it and the answer are dropped, and
/// nothing reaches the shell.
#[test]
fn a_slow_links_answer_ends_the_drain_and_nothing_reaches_the_shell() {
    for round_trip in [200, 500, 900] {
        let mut pty = Pty::spawn("screen");
        pty.assert_up();
        pty.send(b"q");
        assert!(pty.wrote(QUERY), "{:?}", String::from_utf8_lossy(&pty.raw));
        std::thread::sleep(Duration::from_millis(round_trip / 2));
        pty.send(MOUSE_MOVE);
        std::thread::sleep(Duration::from_millis(round_trip / 2));
        pty.send(ANSWER);
        assert!(pty.exit().success());
        pty.assert_restored();
        assert!(
            pty.run_log().contains("answered=true"),
            "{round_trip} ms: {}",
            pty.run_log()
        );
        let raw = String::from_utf8_lossy(&pty.raw).into_owned();
        let after = &raw[raw.find("\x1b[c").expect("the query")..];
        assert!(!after.contains("<35;10;5M"), "{round_trip} ms: {raw:?}");
        assert!(!after.contains("?62;22c"), "{round_trip} ms: {raw:?}");
        pty.assert_nothing_queued();
    }
}

/// A terminal that never answers: the drain reads and drops input for
/// its whole second, so mouse codes inside it never reach the shell.
#[test]
fn a_terminal_that_never_answers_drains_for_the_whole_second() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    pty.send(b"q");
    assert!(pty.wrote(QUERY), "{:?}", String::from_utf8_lossy(&pty.raw));
    let asked = Instant::now();
    for after in [0, 100, 400] {
        std::thread::sleep(Duration::from_millis(after));
        pty.send(MOUSE_MOVE);
    }
    assert!(pty.wrote(b"\x1b[?1049l"));
    let left = asked.elapsed();
    assert!(pty.exit().success());
    pty.assert_restored();
    assert!(
        pty.run_log().contains("answered=false"),
        "{}",
        pty.run_log()
    );
    assert!(left >= Duration::from_millis(950), "{left:?}");
    let raw = String::from_utf8_lossy(&pty.raw).into_owned();
    assert!(!raw.contains("<35;10;5M"), "{raw:?}");
    pty.assert_nothing_queued();
}

/// The mouse in use when the screen is left: no query (a terminal that
/// answers it itself, as mosh does, would end the drain ahead of reports
/// still on their way), the whole 150 ms drained as before.
#[test]
fn with_the_mouse_in_use_the_exit_drains_the_whole_150_ms_without_a_query() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    pty.send(MOUSE_MOVE);
    std::thread::sleep(Duration::from_millis(50));
    pty.send(b"q");
    assert!(pty.wrote(MOUSE_OFF));
    pty.send(MOUSE_MOVE);
    assert!(pty.exit().success());
    pty.assert_restored();
    let raw = String::from_utf8_lossy(&pty.raw).into_owned();
    assert!(!raw.contains("\x1b[c"), "{raw:?}");
    assert!(!raw.contains("<35;10;5M"), "{raw:?}");
    pty.assert_nothing_queued();
    // The whole drain, by the child's own clock: tokio's timeout ends no
    // earlier than its 150 ms.
    let millis = pty.run_log_millis("exit drain, the mouse in use");
    assert!(
        millis >= 150,
        "the drain took {millis} ms:\n{}",
        pty.run_log()
    );
}

/// On the tty the query goes to the tty and its answer is read there,
/// stdout getting none of it.
#[test]
fn on_the_tty_the_exit_drain_ends_at_the_answer() {
    let mut pty = Pty::spawn_on_tty("screen");
    pty.assert_up();
    pty.send(b"q");
    assert!(pty.wrote(QUERY), "{:?}", String::from_utf8_lossy(&pty.raw));
    pty.send(ANSWER);
    assert!(pty.exit().success());
    pty.assert_restored();
    pty.assert_stdout_clean();
    pty.assert_nothing_queued();
    assert!(pty.run_log().contains("answered=true"), "{}", pty.run_log());
}

#[test]
fn dropping_the_screen_restores_the_terminal() {
    let mut pty = Pty::spawn("dropped");
    pty.assert_up();
    pty.send(b"q");
    assert!(pty.exit().success());
    assert!(pty.parser.screen().contents().contains("ending Ok(Quit)"));
    pty.assert_restored();
    assert_cleared_before_leaving(&pty.raw);
}

#[test]
fn ctrl_c_ends_the_screen_as_interrupted() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    pty.send(b"\x03");
    let status = pty.exit();
    assert!(status.success(), "{status:?}");
    assert!(
        pty.parser
            .screen()
            .contents()
            .contains("ending Ok(Interrupted)")
    );
    pty.assert_restored();
}

#[test]
fn sigterm_restores_the_terminal_then_ends_by_the_signal() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    pty.signal(Signal::TERM);
    let status = pty.exit();
    assert!(
        status.signal().is_some_and(|s| s.contains("Terminated")),
        "{status:?}"
    );
    assert!(
        pty.parser
            .screen()
            .contents()
            .contains("ending Ok(Cancelled)")
    );
    pty.assert_restored();
    assert_cleared_before_leaving(&pty.raw);
}

#[test]
fn a_panic_restores_the_terminal_and_reaches_the_run_log() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    pty.send(b"p");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 101, "{status:?}");
    pty.assert_restored();
    // Restored once, before std's message: nothing after the message
    // moves the cursor back over it.
    let raw = String::from_utf8_lossy(&pty.raw);
    assert_eq!(raw.matches("\x1b[?1049l").count(), 1, "{raw:?}");
    let message = raw.find("a planned panic").expect("the message");
    // (ratatui's `Terminal`, dropped later, only shows the cursor.)
    assert!(!raw[message..].contains("\x1b[?1049l"), "{raw:?}");
    assert!(pty.parser.screen().contents().contains("a planned panic"));
    assert!(
        pty.run_log().contains("panicked: a planned panic"),
        "{}",
        pty.run_log()
    );
}

#[test]
fn a_panic_on_another_thread_restores_the_terminal_through_the_hooks() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    pty.send(b"t");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 3, "{status:?}");
    pty.assert_restored();
    assert!(
        pty.run_log().contains("panicked: a thread's panic"),
        "{}",
        pty.run_log()
    );
}

#[test]
fn a_second_signal_on_a_stuck_screen_restores_the_terminal_through_the_hook() {
    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    // The app blocks the runtime's only thread, so the loop never sees the
    // first signal's cancellation.
    pty.send(b"s");
    assert!(pty.showing("stuck"));
    // The first SIGTERM, then more until it ends: each is past cli-core's
    // repeat window as the harness counts, and a late read of one by the
    // signals thread only makes it a copy, so the next one counts.
    pty.signal(Signal::TERM);
    let status = loop {
        std::thread::sleep(PAST_THE_WINDOW);
        if let Some(status) = pty.child.try_wait().expect("wait") {
            break status;
        }
        pty.signal(Signal::TERM);
    };
    pty.exit();
    assert!(
        status.signal().is_some_and(|s| s.contains("Terminated")),
        "{status:?}"
    );
    assert!(!pty.parser.screen().contents().contains("ending"));
    pty.assert_restored();
    assert_cleared_before_leaving(&pty.raw);
}

#[test]
fn on_the_tty_q_leaves_the_screen_and_restores_the_terminal() {
    let mut pty = Pty::spawn_on_tty("screen");
    pty.assert_up();
    pty.send(b"x");
    assert!(pty.showing("keys 1"));
    pty.send(b"q");
    assert!(pty.exit().success());
    pty.assert_restored();
    assert!(pty.stdout().contains("ending Ok(Quit)"), "{}", pty.stdout());
    pty.assert_stdout_clean();
    assert_cleared_before_leaving(&pty.raw);
}

#[test]
fn on_the_tty_dropping_the_screen_restores_the_terminal() {
    let mut pty = Pty::spawn_on_tty("dropped");
    pty.assert_up();
    pty.send(b"q");
    assert!(pty.exit().success());
    pty.assert_restored();
    assert!(pty.stdout().contains("ending Ok(Quit)"), "{}", pty.stdout());
    pty.assert_stdout_clean();
    assert_cleared_before_leaving(&pty.raw);
}

#[test]
fn on_the_tty_a_panic_restores_the_terminal_once() {
    let mut pty = Pty::spawn_on_tty("screen");
    pty.assert_up();
    pty.send(b"p");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 101, "{status:?}");
    pty.assert_restored();
    let raw = String::from_utf8_lossy(&pty.raw);
    assert_eq!(raw.matches("\x1b[?1049l").count(), 1, "{raw:?}");
    let message = raw.find("a planned panic").expect("the message");
    assert!(!raw[message..].contains("\x1b[?1049l"), "{raw:?}");
    assert!(
        pty.run_log().contains("panicked: a planned panic"),
        "{}",
        pty.run_log()
    );
    pty.assert_stdout_clean();
    assert_cleared_before_leaving(&pty.raw);
}

#[test]
fn on_the_tty_a_panic_on_another_thread_restores_the_terminal() {
    let mut pty = Pty::spawn_on_tty("screen");
    pty.assert_up();
    pty.send(b"t");
    let status = pty.exit();
    assert_eq!(status.exit_code(), 3, "{status:?}");
    pty.assert_restored();
    pty.assert_stdout_clean();
}

#[test]
fn on_the_tty_a_second_signal_on_a_stuck_screen_restores_the_terminal() {
    let mut pty = Pty::spawn_on_tty("screen");
    pty.assert_up();
    pty.send(b"s");
    // `stuck` goes to stdout, the file.
    let deadline = Instant::now() + WAIT;
    while !pty.stdout().contains("stuck") {
        assert!(Instant::now() < deadline, "{}", pty.stdout());
        std::thread::sleep(Duration::from_millis(50));
    }
    pty.signal(Signal::TERM);
    let status = loop {
        std::thread::sleep(PAST_THE_WINDOW);
        if let Some(status) = pty.child.try_wait().expect("wait") {
            break status;
        }
        pty.signal(Signal::TERM);
    };
    pty.exit();
    assert!(
        status.signal().is_some_and(|s| s.contains("Terminated")),
        "{status:?}"
    );
    pty.assert_restored();
    pty.assert_stdout_clean();
    assert_cleared_before_leaving(&pty.raw);
}

/// Key to screen change, `count` times; the median. The pause before each
/// key varies over a whole frame (30 ms plus 0 to 16 ms, in steps of
/// 7 ms modulo 17), so the keys don't stay in step with the render
/// interval and the figure covers every phase of it.
fn median_latency(pty: &mut Pty, count: usize) -> Duration {
    let mut times = Vec::with_capacity(count);
    for i in 1..=count {
        let offset = u64::try_from(i * 7 % 17).expect("small");
        std::thread::sleep(Duration::from_millis(30 + offset));
        let start = Instant::now();
        pty.send(b"k");
        let shown = format!("keys {i}");
        assert!(
            pty.wait_for(WAIT, |screen| screen.contents().contains(&shown)),
            "{shown}"
        );
        times.push(start.elapsed());
    }
    times.sort();
    times[count / 2]
}

/// The groundwork's input-latency measurement (tui spec, Testing
/// Strategy): the loop's added latency against the harness's baseline.
/// The bound catches the class of bug proto-logs had (input noticed only
/// on a tick: about 100 ms median added); a frame is 17 ms. The release
/// figure is recorded in the size ADR.
#[test]
fn input_reaches_the_screen_within_a_few_frames() {
    let mut baseline = Pty::spawn("baseline");
    assert!(baseline.showing("keys 0"));
    let base = median_latency(&mut baseline, 30);
    baseline.send(b"q");
    assert!(baseline.exit().success());

    let mut pty = Pty::spawn("screen");
    pty.assert_up();
    let screen = median_latency(&mut pty, 30);
    pty.send(b"q");
    assert!(pty.exit().success());

    let added = screen.saturating_sub(base);
    eprintln!("input latency: median {screen:?} on the screen, {base:?} baseline, {added:?} added");
    assert!(added < Duration::from_millis(100), "{added:?} added");
}

/// The screen's rows erased in place (crossterm's `MoveTo` and
/// `Clear(CurrentLine)`, ESC [2K) after its last frame and before the
/// alternate screen is left, and no ESC [2J after the frame: GNU screen and
/// tmux without an alternate screen move a frame erased by ESC [2J into
/// their scrollback (docs/platform-notes/terminal-clear-scrollback.md), so
/// a terminal without one keeps no frame with plain values (the
/// maintainer, 2026-11-19: every screen is left blank; tui spec, "Keys: one map for
/// every screen", decision 6).
fn assert_cleared_before_leaving(raw: &[u8]) {
    let raw = String::from_utf8_lossy(raw);
    let leave = raw.rfind("\x1b[?1049l").expect("the alternate screen left");
    let drawn = ["keys", "stuck"]
        .iter()
        .filter_map(|text| raw[..leave].rfind(text))
        .max()
        .expect("a frame");
    let after = &raw[drawn..leave];
    assert!(
        after.matches("\x1b[2K").count() >= 24,
        "each of the 24 rows erased after the last frame: {raw:?}"
    );
    assert!(!after.contains("\x1b[2J"), "no erase-display: {raw:?}");
}
