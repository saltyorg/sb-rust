//! The terminal sb draws on, while the runner is live (term-ui spec,
//! "Terminal restore"): the control-character echo (`ECHOCTL`) off, so
//! Ctrl-C doesn't echo `^C` into the tree, and the cursor hidden; both put
//! back at every ending, and by cli-core's restore hook on a second Ctrl-C.
//!
//! proto-runner's reviewed guard (tcgetattr/tcsetattr through rustix's
//! termios wrappers; UNVERIFIED there against tmux), with two changes: the
//! terminal is stderr's, which the runner draws on, not stdin's (a pipe
//! under `curl … | sudo bash`); and it is touched only from the terminal's
//! foreground process group (sb-go's `tty.go` guard), so a background sb
//! never stops on SIGTTOU. Why not crossterm's `enable_raw_mode`: raw mode
//! clears ISIG, so Ctrl-C would stop sending SIGINT.
//! https://docs.rs/rustix/1.1.6/rustix/termios/fn.tcsetattr.html

use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcgetpgrp, tcsetattr};

/// Shows the cursor (DECTCEM), the bytes console's `Term::show_cursor`
/// writes. Why not that call: it writes through `io::stderr()` (console
/// 0.16.6 term.rs `write_through_common`), whose lock cli-core's restore
/// hooks must not take.
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
/// Hides the cursor.
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";

/// Whether sb is in the foreground of stderr's terminal: only then may it
/// change the terminal's modes (from the background, `tcsetattr` stops the
/// process with SIGTTOU). Checked again at every restore, since a user can
/// move sb to the background mid-run (Ctrl-Z, then `bg`).
fn in_foreground() -> bool {
    tcgetpgrp(rustix::stdio::stderr()).is_ok_and(|group| group == rustix::process::getpgrp())
}

/// What the runner changed on the terminal, to put back: the modes it
/// found, or nothing when it changed nothing (not a terminal, or not in
/// the foreground), or once it has put them back.
#[derive(Clone, Default)]
pub(crate) struct Saved(Option<Termios>);

impl Saved {
    /// Quiet the terminal on stderr, if it is one and sb is in its
    /// foreground: `ECHOCTL` off, the cursor hidden.
    pub fn quiet() -> Self {
        let stderr = rustix::stdio::stderr();
        if !in_foreground() {
            return Saved(None);
        }
        let Ok(original) = tcgetattr(stderr) else {
            return Saved(None);
        };
        let mut quiet = original.clone();
        quiet.local_modes.remove(LocalModes::ECHOCTL);
        if tcsetattr(stderr, OptionalActions::Now, &quiet).is_err() {
            return Saved(None);
        }
        write_all(HIDE_CURSOR);
        Saved(Some(original))
    }

    /// Put the terminal back, once: the cursor shown and its modes, the
    /// modes only from the foreground (in the background a job-control
    /// shell puts its own modes back, and `fg` restores the job's). Writes
    /// through stderr's descriptor, not std's `stderr()`, whose lock a
    /// stuck writer may hold (cli-core's restore hooks run beside it).
    pub fn restore(&mut self) {
        self.put_back(false);
    }

    /// After a forced exit: also end the live area's last line, so the
    /// shell's prompt starts on a line of its own.
    pub fn restore_for_exit(&mut self) {
        self.put_back(true);
    }

    /// Nothing at all when nothing was changed (or it was put back).
    fn put_back(&mut self, end_line: bool) {
        if let Some(original) = self.0.take() {
            if end_line {
                write_all(b"\r\n");
            }
            write_all(SHOW_CURSOR);
            if in_foreground() {
                let _ = tcsetattr(rustix::stdio::stderr(), OptionalActions::Now, &original);
            }
        }
    }

    /// Whether anything was changed and not yet put back.
    pub fn changed(&self) -> bool {
        self.0.is_some()
    }
}

/// Write all of `bytes` to stderr's descriptor, giving up on an error
/// (nothing better to do while restoring).
fn write_all(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match rustix::io::write(rustix::stdio::stderr(), bytes) {
            Ok(0) | Err(_) => return,
            Ok(written) => bytes = &bytes[written..],
        }
    }
}
