//! PTY children through portable-pty (spec, `pty`): for tools that colour
//! or show progress only on a terminal. The child gets a pseudo-terminal
//! of sb's terminal size as its stdin, stdout and stderr, in a session of
//! its own (portable-pty's `setsid`, so the session's group id is the
//! child's pid); its lines go to the step as `stream`'s do.
//! (<https://docs.rs/portable-pty/0.9.0/portable_pty/>)

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Instant;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::Write;

use rustix::io::Errno;
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, WaitOptions, kill_process, waitid, waitpid,
};
use sb_term_ui::Step;
use tokio::sync::{mpsc, oneshot};

use crate::group::Group;
use crate::mask::Mask;
use crate::run::{CHUNK, LINE, Leader, QUEUED, check_dir, log_end, shown_line};
use crate::{Cmd, Outcome, Ran, SpawnCause, SpawnError, Way};

/// The PTY's reader: portable-pty's, a blocking std `Read`.
type PtyReader = Box<dyn Read + Send>;

/// The PTY's size when stderr isn't a terminal (sb-go's fixed 24×120).
const FALLBACK: PtySize = PtySize {
    rows: 24,
    cols: 120,
    pixel_width: 0,
    pixel_height: 0,
};

impl Cmd {
    /// Run the child on a pseudo-terminal of sb's terminal size (24×120
    /// when stderr isn't one), each line of its output handed to `step`
    /// as `stream` does, secrets taken out. Its stdin is the PTY, given the
    /// end-of-file character at once, so a reading child's first read is
    /// end of file; `stdin(bytes)` is refused. A child that flushes its
    /// input before prompting (a password prompt) still waits: a PTY
    /// child must not prompt, so give it a non-interactive flag or a
    /// timeout. The working directory is always set, and checked first
    /// (portable-pty would fall back to `$HOME`). The step's token stops
    /// the session's group as `capture`'s does.
    ///
    /// # Errors
    ///
    /// [`SpawnError`] when the child can't start: the working directory
    /// isn't one or can't be looked at, or the operating system refuses, or
    /// the PTY can't be opened; also when stdin bytes were given
    /// ([`SpawnCause::StdinRefused`]).
    pub async fn pty(&self, step: &mut Step) -> Result<Ran, SpawnError> {
        let command = self.shown();
        let token = step.token();
        let (mut leader, group, reader) = self.spawn_pty()?;
        let start = Instant::now();
        let mask = self.mask();
        let (lines, mut queued) = mpsc::channel::<Vec<u8>>(QUEUED);
        // The reader blocks on the PTY, so it runs on a thread of its own
        // (portable-pty's reader is std `Read`), handing lines over as
        // `stream`'s readers do. A background process still holding the
        // PTY after the run keeps the thread until it lets go.
        // Why not tokio's `AsyncFd` on the PTY: portable-pty hands out only
        // a std `Read` (and a raw fd, which borrowing needs `unsafe` for).
        let window = LINE + mask.longest();
        let reading = std::thread::Builder::new()
            .name("pty reader".into())
            .spawn(move || read_pty_lines(reader, &mask, window, &lines));
        if let Err(err) = reading {
            // No reader: nothing would see the child's output; it goes.
            tracing::warn!(command, error = %err, "pty reader not started");
            let _ = kill_process(leader.pid, Signal::KILL);
            return Err(self.failed(err.into()));
        }
        let io = async {
            while let Some(line) = queued.recv().await {
                step.output(String::from_utf8_lossy(&line).into_owned());
            }
        };
        let ending = self.supervise(&mut leader, group, &token, io).await?;
        let outcome = Outcome {
            ending,
            elapsed: start.elapsed(),
        };
        log_end(&command, &outcome);
        Ok(Ran { outcome, command })
    }

    /// Open the PTY and spawn the child on it, logging its start (or why it
    /// didn't start); the PTY's reader comes back with the leader.
    fn spawn_pty(&self) -> Result<(PtyLeader, Option<Group>, PtyReader), SpawnError> {
        match self.pty_child() {
            Ok((leader, reader)) => {
                let group = Group::of_pid(leader.pid);
                self.log_start(Way::Pty);
                Ok((leader, Some(group), reader))
            }
            Err(cause) => {
                let command = self.shown();
                let error: &(dyn std::error::Error + 'static) = &cause;
                tracing::warn!(command, error, "child not started");
                Err(SpawnError {
                    program: self.program.to_string_lossy().into_owned(),
                    cause,
                })
            }
        }
    }

    fn pty_child(&self) -> Result<(PtyLeader, PtyReader), SpawnCause> {
        if self.stdin.is_some() {
            return Err(SpawnCause::StdinRefused("pty"));
        }
        let dir = match &self.cwd {
            Some(dir) => dir.clone(),
            None => std::env::current_dir()?,
        };
        check_dir(&dir)?;
        let pair = native_pty_system()
            .openpty(size())
            .map_err(|err| pty_error(&err))?;
        // Fail closed: without its name its echo can't be turned off, and
        // the end-of-file character would show in the output.
        let tty = pair
            .master
            .tty_name()
            .ok_or_else(|| std::io::Error::other("the PTY has no name"))?;
        let eof = prepare_tty(&tty)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "sb-exec is where every child process starts, a PTY's too (CONSTRAINTS.md E4)"
        )]
        let mut builder = CommandBuilder::new(&self.program);
        for arg in &self.args {
            builder.arg(arg.text());
        }
        let environment = self.environment(Way::Pty);
        if environment.clear {
            builder.env_clear();
        }
        for (name, value) in environment.vars {
            match value {
                Some(value) => builder.env(name, value),
                None => builder.env_remove(name),
            }
        }
        builder.cwd(&dir);
        // The end-of-file character alone, at the start of an empty line,
        // is end of file for a reading child; written before the spawn, it
        // waits in the PTY for the child's first read. The writer stays
        // open until the child is reaped: dropping it sends a newline first
        // (portable-pty's `UnixMasterWriter` drop, src/unix.rs), which a
        // reading child would take as an empty answer. Taken before the
        // spawn too, so no error can leave a started child behind.
        let mut writer = pair.master.take_writer().map_err(|err| pty_error(&err))?;
        writer.write_all(&[eof])?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| pty_error(&err))?;
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|err| pty_error(&err))?;
        drop(pair.slave);
        let pid = child
            .process_id()
            .and_then(|pid| Pid::from_raw(i32::try_from(pid).ok()?))
            .ok_or_else(|| std::io::Error::other("the PTY child has no pid"))?;
        // Waited on here rather than through portable-pty's `wait`, whose
        // ExitStatus gives a signal's description only ("Terminated"):
        // rustix keeps the number (signal-hook names it). Why not downcast
        // its child to `std::process::Child` and wait on that: the concrete
        // type is portable-pty's undocumented detail. The exit is seen
        // without reaping (`waitid` with `NOWAIT`), and the child reaped
        // only once supervise has seen it (or the run is dropped): until
        // then the zombie keeps the pid and the group id sb may signal.
        let (exited, exit) = oneshot::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let master = pair.master;
        let waiting = std::thread::Builder::new()
            .name("pty wait".into())
            .spawn(move || {
                let _ = exited.send(exit_status(pid));
                let _ = released.recv();
                reap(pid);
                drop(writer);
                drop(master);
                drop(child);
            });
        if let Err(err) = waiting {
            // Nothing would reap or stop it: it goes now.
            let _ = kill_process(pid, Signal::KILL);
            reap(pid);
            return Err(err.into());
        }
        Ok((
            PtyLeader {
                pid,
                exit,
                release: Some(release),
            },
            reader,
        ))
    }
}

/// Turn the PTY's echo off before the child starts, and give back its
/// end-of-file character. With echo on, the newline portable-pty's writer
/// sends when dropped (after the reap) would come back as an empty output
/// line while a background process still holds the PTY (the end-of-file
/// character itself isn't echoed); before the end of file was written
/// alone, the echo also split the child's first line under load. Its stdin is never
/// typed into, so nothing else is echoed. Opened by its name without
/// becoming sb's controlling terminal (`O_NOCTTY`), through rustix's safe
/// termios calls
/// (<https://docs.rs/rustix/1.1.6/rustix/termios/fn.tcsetattr.html>); why
/// not portable-pty: it only reads the modes (`get_termios`), with no
/// setter.
fn prepare_tty(tty: &std::path::Path) -> std::io::Result<u8> {
    use rustix::fs::{Mode, OFlags, open};
    use rustix::termios::{LocalModes, OptionalActions, SpecialCodeIndex, tcgetattr, tcsetattr};
    let fd = open(
        tty,
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut modes = tcgetattr(&fd)?;
    modes.local_modes.remove(LocalModes::ECHO);
    tcsetattr(&fd, OptionalActions::Now, &modes)?;
    Ok(modes.special_codes[SpecialCodeIndex::VEOF])
}

/// Wait for `pid` to exit without reaping it (`waitid` with `NOWAIT`),
/// as std's `ExitStatus`.
fn exit_status(pid: Pid) -> std::io::Result<ExitStatus> {
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    let status = loop {
        match waitid(WaitId::Pid(pid), options) {
            Err(Errno::INTR) => {}
            other => break other?,
        }
    };
    let status = status.ok_or_else(|| std::io::Error::other("no exit status"))?;
    Ok(match (status.exit_status(), status.terminating_signal()) {
        (Some(code), _) => ExitStatus::from_raw((code & 0xff) << 8),
        (None, Some(signal)) => ExitStatus::from_raw(signal & 0x7f),
        (None, None) => {
            return Err(std::io::Error::other(
                "an exit status with no code or signal",
            ));
        }
    })
}

/// Reap `pid` (it has exited, or been killed).
fn reap(pid: Pid) {
    while let Err(Errno::INTR) = waitpid(Some(pid), WaitOptions::empty()) {}
}

/// sb's terminal size (stderr's), or the fallback when it isn't one.
fn size() -> PtySize {
    rustix::termios::tcgetwinsize(std::io::stderr()).map_or(FALLBACK, |size| {
        if size.ws_row == 0 || size.ws_col == 0 {
            FALLBACK
        } else {
            PtySize {
                rows: size.ws_row,
                cols: size.ws_col,
                pixel_width: 0,
                pixel_height: 0,
            }
        }
    })
}

/// portable-pty reports through anyhow. A program it can't find is
/// `NotFound`, as capture's (its message would carry the whole `PATH`);
/// otherwise its message, on one line.
fn pty_error(err: &anyhow::Error) -> SpawnCause {
    let message = format!("{err:#}");
    // portable-pty 0.9.0's wording (src/cmdbuilder.rs, `search_path`);
    // the tests catch a change.
    if [
        "No viable candidates found in PATH",
        "does not exist",
        "doesn't exist on the filesystem",
    ]
    .iter()
    .any(|text| message.contains(text))
    {
        return SpawnCause::Io(std::io::ErrorKind::NotFound.into());
    }
    if message.contains("is not executable") {
        return SpawnCause::Io(std::io::ErrorKind::PermissionDenied.into());
    }
    let one_line: Vec<&str> = message.lines().map(str::trim).collect();
    SpawnCause::Io(std::io::Error::other(one_line.join(" ")))
}

/// The PTY child's leader, waited on by its own thread, which reaps it
/// once released: by `wait`, or by this being dropped (the sender with
/// it). A dropped run drops its `Group` (inside the supervise future)
/// before this, so the group's SIGKILL comes while the zombie still holds
/// the id.
struct PtyLeader {
    pid: Pid,
    exit: oneshot::Receiver<std::io::Result<ExitStatus>>,
    release: Option<std::sync::mpsc::Sender<()>>,
}

impl Leader for PtyLeader {
    /// The wait thread's result; awaiting the receiver is cancel safe
    /// (<https://docs.rs/tokio/1.54.1/tokio/sync/oneshot/struct.Receiver.html>).
    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let status = (&mut self.exit)
            .await
            .unwrap_or_else(|_| Err(std::io::Error::other("the PTY wait thread ended")));
        // Seen: supervise signals neither the pid nor the group from now,
        // so the zombie can go (left, it would count as a group member).
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        status
    }

    /// Nothing beyond the group's SIGKILL: a session's leader can't leave
    /// its group, so that already reaches it.
    fn kill(&mut self) {
        let _ = self.pid;
    }

    /// Nothing: a PTY child always has its session's group, whose stop
    /// reaches the leader (`supervise` stops by pid only without one).
    fn terminate(&mut self) {}
}

/// Read the PTY line by line to its end (portable-pty's reader turns the
/// EIO of a closed child side into end of file), each line cut, masked
/// and sent as `stream`'s are. std's
/// `read_until` through `take` keeps each read bounded
/// (<https://doc.rust-lang.org/std/io/trait.BufRead.html#method.read_until>).
fn read_pty_lines(reader: PtyReader, mask: &Mask, window: usize, lines: &mpsc::Sender<Vec<u8>>) {
    let mut reader = BufReader::new(reader);
    let limit = u64::try_from(window + 1).unwrap_or(u64::MAX);
    loop {
        let mut line = Vec::new();
        match (&mut reader).take(limit).read_until(b'\n', &mut line) {
            Ok(1..) => {}
            Ok(0) | Err(_) => return,
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        } else if line.len() > window {
            // A PTY that ends inside the dropped rest reads 0 next.
            skip_pty_line(&mut reader);
        }
        if lines.blocking_send(shown_line(&line, mask)).is_err() {
            return;
        }
    }
}

/// Drop the rest of a line past its cut; false if the PTY ended first.
fn skip_pty_line(reader: &mut BufReader<PtyReader>) -> bool {
    let mut rest = Vec::new();
    let limit = u64::try_from(CHUNK).unwrap_or(u64::MAX);
    loop {
        rest.clear();
        match (&mut *reader).take(limit).read_until(b'\n', &mut rest) {
            Ok(1..) if rest.last() == Some(&b'\n') => return true,
            Ok(1..) => {}
            Ok(0) | Err(_) => return false,
        }
    }
}
