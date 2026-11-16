//! Shared by `sb setup`'s and `sb update`'s end-to-end tests: a command
//! (any `std::process::Command`, so sb in sb-ops' namespace too) on a PTY
//! of its own, its output gathered raw as it arrives, for questions asked
//! on the terminal.

use std::io::{Read as _, Write as _};
use std::process::Command;
use std::time::{Duration, Instant};

/// A command on a PTY of its own (portable-pty, as `logs_pty`'s): its
/// output gathered as it arrives, raw.
pub struct Pty {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    // The master kept open while the child runs.
    _master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    output: std::sync::mpsc::Receiver<Vec<u8>>,
    seen: Vec<u8>,
}

impl Pty {
    /// How long a test waits for a screen or an exit.
    const WAIT: Duration = Duration::from_secs(60);

    /// `cmd` (its program, arguments and environment) on a new PTY.
    pub fn spawn(cmd: &Command) -> Self {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("pty");
        let mut builder = CommandBuilder::new(cmd.get_program());
        builder.args(cmd.get_args());
        builder.env("TERM", "xterm-256color");
        for (name, value) in cmd.get_envs() {
            match value {
                Some(value) => builder.env(name, value),
                None => builder.env_remove(name),
            }
        }
        let child = pair.slave.spawn_command(builder).expect("spawn");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let writer = pair.master.take_writer().expect("writer");
        let (tx, output) = std::sync::mpsc::channel();
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
            seen: Vec::new(),
        }
    }

    /// Everything the command wrote so far, as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.seen).into_owned()
    }

    /// Wait until the output holds `text`; false at the timeout.
    pub fn showing(&mut self, text: &str) -> bool {
        let deadline = Instant::now() + Self::WAIT;
        while !self.text().contains(text) {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.output.recv_timeout(left) {
                Ok(bytes) => self.seen.extend(bytes),
                Err(_) => return self.text().contains(text),
            }
        }
        true
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write");
        self.writer.flush().expect("flush");
    }

    /// Wait for the command to exit, gathering its output meanwhile.
    pub fn exit(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + Self::WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                while let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(200)) {
                    self.seen.extend(bytes);
                }
                return status;
            }
            assert!(Instant::now() < deadline, "no exit:\n{}", self.text());
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(10)) {
                self.seen.extend(bytes);
            }
        }
    }
}
