//! Piped children through `tokio::process` (spec, "Ways to run"):
//! `capture` and `stream`.

use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use sb_cli_core::cancel::Shutdown;
use sb_cli_core::config::MAX_FILE_BYTES;
use sb_term_ui::Step;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::group::{AttachedPid, Group};
use crate::mask::Mask;
use crate::{Captured, Cmd, Ending, Outcome, Ran, SpawnCause, SpawnError, Streamed, Way};

/// How long output is still read after the child exits: a background
/// grandchild can hold the pipe open (spec, "After the child exits"; sb-go
/// handles it with `WaitDelay`).
const DRAIN: Duration = Duration::from_secs(2);

/// Bytes read from a pipe at a time.
pub(crate) const CHUNK: usize = 8 * 1024;

/// The longest streamed line; the rest of a longer one is dropped up to
/// its end (spec, "Lines").
pub(crate) const LINE: usize = 4 * 1024;

/// How much of stderr `stream` keeps for its caller.
const STDERR_TAIL: usize = 64 * 1024;

/// Lines read but not yet handed to the step.
pub(crate) const QUEUED: usize = 64;

/// A stop under way: why (the outcome it gives), and when the group gets
/// SIGKILL.
struct Stop {
    ending: Ending,
    kill_at: tokio::time::Instant,
    killed: bool,
}

impl Stop {
    /// Begin stopping now: SIGTERM and SIGCONT to the group.
    fn now(ending: Ending, grace: Duration, group: Option<&mut Group>) -> Self {
        if let Some(group) = group {
            group.terminate();
        }
        Self {
            ending,
            kill_at: tokio::time::Instant::now() + grace,
            killed: false,
        }
    }
}

/// Sleep until `at`, or for ever without one (a branch its guard keeps
/// off).
async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// One stream's bytes as kept, and whether any were left out. A deque, so
/// the tail drops its front in place, as its reads arrive.
#[derive(Default)]
struct Kept {
    bytes: VecDeque<u8>,
    cut: bool,
}

impl Cmd {
    /// Run the child to its end and return its outcome and output (spec,
    /// `capture`): stdout and stderr read concurrently, each up to the
    /// 1 MiB cap (the first of stdout, the last of stderr), the child in a
    /// process group of its own, stdin null unless [`Cmd::stdin`] gave
    /// bytes. Why not tokio's `Child::wait_with_output`: it keeps all the
    /// output, with no cap, and waits for the pipes to close, which a
    /// background grandchild can put off for good. Cancelling `token`, or
    /// the timeout passing, ends the child's group (SIGTERM, the grace,
    /// SIGKILL) and the outcome is interrupted or timed out.
    ///
    /// # Errors
    ///
    /// [`SpawnError`] when the child can't start: the working directory
    /// isn't one or can't be looked at, or the operating system refuses
    /// (not found, not executable, ...).
    pub async fn capture(&self, token: &CancellationToken) -> Result<Captured, SpawnError> {
        let command = self.shown();
        let (mut child, group) = self.spawn(Way::Capture)?;
        let start = Instant::now();
        let mask = self.mask();
        let keep = MAX_FILE_BYTES + mask.longest();
        let (mut stdout, mut stderr) = (Kept::default(), Kept::default());
        let stdin = (child.stdin.take(), self.stdin.as_deref());
        let pipes = (child.stdout.take(), child.stderr.take());
        let io = async {
            tokio::join!(
                write_stdin(stdin.0, stdin.1),
                read_head(pipes.0, keep, &mut stdout),
                read_tail(pipes.1, keep, &mut stderr),
            );
        };
        let ending = self.supervise(&mut child, group, token, io).await?;
        let outcome = Outcome {
            ending,
            elapsed: start.elapsed(),
        };
        log_end(&command, &outcome);
        // Past the cap, though maybe within the window kept for secrets.
        let truncated = [&stdout, &stderr]
            .iter()
            .any(|kept| kept.cut || kept.bytes.len() > MAX_FILE_BYTES);
        Ok(Captured {
            outcome,
            stdout: head_text(&mask, stdout.bytes.make_contiguous()),
            stderr: tail_text(&mask, stderr.bytes.make_contiguous()),
            truncated,
            command,
        })
    }

    /// Run the child to its end with each line of its output handed to
    /// `step` as it arrives (spec, `stream`), secrets taken out, and
    /// stderr's last 64 KiB kept for the caller. The step's token stops it
    /// as `capture`'s does. Lines are split from bytes at `\n` with tokio's
    /// `read_until` through `take`, so each read is bounded
    /// (<https://docs.rs/tokio/1.54.1/tokio/io/trait.AsyncBufReadExt.html#method.read_until>);
    /// one reader per pipe, each in its own loop (a `read_until` dropped
    /// by a `select!` keeps its partial line only with the same buffer),
    /// handing lines through tokio's bounded channel
    /// (<https://docs.rs/tokio/1.54.1/tokio/sync/mpsc/fn.channel.html>) to the
    /// one future holding the step. A slow step (its log write) holds the
    /// readers back once the channel is full, and the child then waits on a
    /// full pipe: bounded, by design. Why not tokio-util's `LinesCodec` or
    /// `AnyDelimiterCodec` with a maximum length (0.7.20): on a longer line
    /// they drop it whole, its first 4 KiB too, and report an error, after
    /// which `FramedRead` ends (`src/codec/lines_codec.rs`, the
    /// `is_discarding` arms; `src/codec/framed_impl.rs`, `has_errored`); the
    /// spec keeps the first 4 KiB and masks across the cut.
    /// Secrets are taken out with exec's mask before the step sees the
    /// line, so the screen and the log get the same text; why not
    /// `Line::secret`: it needs the secret's place in the line, which the
    /// mask already finds, and gives the same result.
    ///
    /// # Errors
    ///
    /// [`SpawnError`] when the child can't start: the working directory
    /// isn't one or can't be looked at, or the operating system refuses
    /// (not found, not executable, ...).
    pub async fn stream(&self, step: &mut Step) -> Result<Streamed, SpawnError> {
        let command = self.shown();
        let token = step.token();
        let (mut child, group) = self.spawn(Way::Stream)?;
        let start = Instant::now();
        let mask = self.mask();
        let mut tail = Kept::default();
        let stdin = (child.stdin.take(), self.stdin.as_deref());
        let pipes = (child.stdout.take(), child.stderr.take());
        let (lines, mut queued) = mpsc::channel::<Vec<u8>>(QUEUED);
        // Each reader's line so far, outside the readers: when the drain
        // ends the read (a grandchild holding the pipe), what they had
        // read of a last line without `\n` is still delivered (exec-L2).
        let (mut out_line, mut err_line) = (Vec::new(), Vec::new());
        let io = async {
            let shown = async {
                while let Some(line) = queued.recv().await {
                    step.output(String::from_utf8_lossy(&line).into_owned());
                }
            };
            tokio::join!(
                write_stdin(stdin.0, stdin.1),
                read_lines(pipes.0, &mask, lines.clone(), None, &mut out_line),
                read_lines(pipes.1, &mask, lines, Some(&mut tail), &mut err_line),
                shown,
            );
        };
        let ending = self.supervise(&mut child, group, &token, io).await?;
        // After a drain that ended the read: the lines queued but not yet
        // shown, then each pipe's unfinished one. Empty otherwise.
        while let Ok(line) = queued.try_recv() {
            step.output(String::from_utf8_lossy(&line).into_owned());
        }
        for (line, kept) in [(&out_line, None), (&err_line, Some(&mut tail))] {
            if !line.is_empty() {
                let shown = keep_line(line, &mask, kept);
                step.output(String::from_utf8_lossy(&shown).into_owned());
            }
        }
        let outcome = Outcome {
            ending,
            elapsed: start.elapsed(),
        };
        log_end(&command, &outcome);
        Ok(Streamed {
            outcome,
            stderr_tail: String::from_utf8_lossy(tail.bytes.make_contiguous()).into_owned(),
            command,
        })
    }

    /// Run the child on sb's own stdin, stdout and stderr, in sb's process
    /// group, to its end (spec, `attached`): nothing is collected or
    /// logged of its output; it can't start while a term-ui runner draws,
    /// nor take stdin bytes. While it runs, cli-core holds SIGINT and
    /// SIGQUIT ("wait and cooperative exit", decision 2): the terminal
    /// delivers them to the child, and sb only notes them; if the child
    /// then dies of one sb received, the run is interrupted (the outcome
    /// interrupted, sb ending by that signal), else it is the child's own
    /// ending. A SIGTERM to sb cancels the token, which sends SIGTERM to
    /// the child's pid, then SIGKILL after the grace; the timeout does
    /// the same. sb's forced exit SIGKILLs the pid (decision 12). Starting
    /// and ending the hold, and a signalled child's question (up to
    /// 100 ms), block this thread briefly (cli-core's `Shutdown::hold`):
    /// accepted, as nothing else draws meanwhile. Stopped by its pid
    /// alone, a child's own children can outlive it (spec, known limits).
    ///
    /// # Errors
    ///
    /// [`SpawnError`] when the child can't start: the working directory
    /// isn't one or can't be looked at, or the operating system refuses;
    /// also when stdin bytes were given ([`SpawnCause::StdinRefused`]) or a
    /// term-ui runner is drawing ([`SpawnCause::RunnerLive`]).
    pub async fn attached(&self, shutdown: &Shutdown) -> Result<Ran, SpawnError> {
        let command = self.shown();
        // Refused before the hold (whose start can wait) and the spawn.
        if let Err(cause) = self.attached_refusal() {
            return Err(self.not_started(cause));
        }
        // An interactive child started only to be stopped at once (an
        // editor flashing up): not started, the outcome interrupted.
        if shutdown.token().is_cancelled() {
            tracing::info!(command, "child not started: the run was interrupted");
            return Ok(Ran {
                outcome: Outcome {
                    ending: Ending::Interrupted,
                    elapsed: Duration::ZERO,
                },
                command,
            });
        }
        let hold = shutdown.hold();
        let (mut child, _) = self.spawn(Way::Attached)?;
        let registered = AttachedPid::new(child.id());
        let start = Instant::now();
        let token = shutdown.token();
        let ending = self
            .supervise(&mut child, None, &token, std::future::ready(()))
            .await?;
        drop(registered);
        let ending = match ending {
            Ending::Signalled(signal) if hold.child_ended_by(Some(signal)) => Ending::Interrupted,
            ending => ending,
        };
        drop(hold);
        let outcome = Outcome {
            ending,
            elapsed: start.elapsed(),
        };
        log_end(&command, &outcome);
        Ok(Ran { outcome, command })
    }

    /// Log why the child didn't start, and its error. As an error value,
    /// so its causes are logged too (`error.sources`, as cli-core's run
    /// record).
    fn not_started(&self, cause: SpawnCause) -> SpawnError {
        let command = self.shown();
        let error: &(dyn std::error::Error + 'static) = &cause;
        tracing::warn!(command, error, "child not started");
        self.failed(cause)
    }

    /// Why an attached child can't start (decision 11): its stdin is sb's
    /// own, and it can't share the terminal with a runner.
    fn attached_refusal(&self) -> Result<(), SpawnCause> {
        if self.stdin.is_some() {
            return Err(SpawnCause::StdinRefused("attached"));
        }
        if sb_term_ui::runner_live() {
            return Err(SpawnCause::RunnerLive);
        }
        Ok(())
    }

    /// The error for a child that couldn't run, naming the program.
    pub(crate) fn failed(&self, cause: SpawnCause) -> SpawnError {
        SpawnError {
            program: self.program.to_string_lossy().into_owned(),
            cause,
        }
    }

    /// Spawn the child for `way` in a group of its own and log its start,
    /// or log why it didn't start.
    fn spawn(&self, way: Way) -> Result<(Child, Option<Group>), SpawnError> {
        let spawned = self
            .command(way)
            .and_then(|mut child| child.spawn().map_err(SpawnCause::from));
        match spawned {
            Ok(child) => {
                // An attached child stays in sb's group: none of its own.
                let group = (way != Way::Attached)
                    .then(|| child.id().and_then(Group::of))
                    .flatten();
                self.log_start(way);
                Ok((child, group))
            }
            Err(cause) => Err(self.not_started(cause)),
        }
    }

    /// Run `io` (the child's pipes) alongside the child to its end: on
    /// cancellation or the timeout the group's stop (SIGTERM, the grace,
    /// SIGKILL; by the leader's pid when there is no group), what is left
    /// of it after the leader's reap, then the output still coming for up
    /// to [`DRAIN`]. Interrupted and timed out win over the child's own
    /// ending.
    pub(crate) async fn supervise(
        &self,
        child: &mut impl Leader,
        mut group: Option<Group>,
        token: &CancellationToken,
        io: impl Future<Output = ()>,
    ) -> Result<Ending, SpawnError> {
        tokio::pin!(io);
        let mut drained = false;
        let mut stop: Option<Stop> = None;
        let time_out_at = self
            .timeout
            .map(|limit| tokio::time::Instant::now() + limit);
        let status = loop {
            let kill_at = stop.as_ref().map(|stop| stop.kill_at);
            tokio::select! {
                status = child.wait() => break status,
                () = &mut io, if !drained => drained = true,
                () = token.cancelled(), if stop.is_none() => {
                    stop = Some(Stop::now(Ending::Interrupted, self.grace, group.as_mut()));
                    // Without a group (an attached child), by pid.
                    if group.is_none() {
                        child.terminate();
                    }
                }
                () = sleep_until(time_out_at), if stop.is_none() && time_out_at.is_some() => {
                    stop = Some(Stop::now(Ending::TimedOut, self.grace, group.as_mut()));
                    if group.is_none() {
                        child.terminate();
                    }
                }
                () = sleep_until(kill_at), if stop.as_ref().is_some_and(|stop| !stop.killed) => {
                    if let Some(group) = &group {
                        group.kill();
                    }
                    // The leader's pid too: it may have left its group,
                    // and it is unreaped, so the pid is still its own.
                    child.kill();
                    if let Some(stop) = &mut stop {
                        stop.killed = true;
                    }
                }
            }
        };
        if let Some(group) = &mut group {
            group.reaped();
        }
        match (&stop, &group) {
            (Some(stop), Some(group)) => {
                // Whatever is left of the group, within the grace plus a
                // second, its output read meanwhile; then what is still
                // coming, for up to the drain, as after any exit: `stream`
                // hands lines over through a channel, so its reads can be
                // well behind when the group is gone.
                let finishing = group.finish(stop.kill_at, stop.killed);
                tokio::pin!(finishing);
                loop {
                    tokio::select! {
                        () = &mut finishing => break,
                        () = &mut io, if !drained => drained = true,
                    }
                }
                if !drained {
                    let _ = tokio::time::timeout(DRAIN, &mut io).await;
                }
            }
            _ if !drained => {
                let _ = tokio::time::timeout(DRAIN, &mut io).await;
            }
            _ => {}
        }
        let status = status.map_err(|err| self.failed(err.into()))?;
        Ok(stop.map_or_else(|| ending(status), |stop| stop.ending))
    }
}

/// A child's leader as `supervise` needs it: wait for its exit (cancel
/// safe, as it sits in a `select!`), and SIGKILL its pid while unreaped.
pub(crate) trait Leader {
    async fn wait(&mut self) -> std::io::Result<ExitStatus>;
    fn kill(&mut self);
    /// SIGTERM (and SIGCONT, as a group's stop) to the leader's pid, for a
    /// child without a group of its own: an attached one, or a piped one
    /// whose group couldn't be found (intended there too).
    fn terminate(&mut self);
}

impl Leader for Child {
    /// tokio's own wait, documented cancel safe
    /// (<https://docs.rs/tokio/1.54.1/tokio/process/struct.Child.html#method.wait>).
    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        Child::wait(self).await
    }

    fn kill(&mut self) {
        let _ = self.start_kill();
    }

    /// rustix's `kill_process`, as the groups' signals
    /// (<https://docs.rs/rustix/1.1.6/rustix/process/fn.kill_process.html>);
    /// the pid is unreaped while tokio's `Child` hasn't seen it exit
    /// (`id` is `None` after). Why not tokio's `start_kill` or std's
    /// `send_signal`: the first is SIGKILL only, the second unstable on
    /// Rust 1.100.0.
    fn terminate(&mut self) {
        if let Some(pid) = self.id().and_then(crate::group::pid_of) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::CONT);
        }
    }
}

impl Cmd {
    /// tokio's `Command` for `way`: program, arguments, the environment
    /// rules, working directory, and (but for an attached child, which
    /// keeps sb's stdio and group) piped output and its own process group
    /// (`process_group(0)`,
    /// <https://docs.rs/tokio/1.54.1/tokio/process/struct.Command.html#method.process_group>),
    /// killed if the run is dropped before it ends (`kill_on_drop`). The
    /// working directory is checked first: a missing one would otherwise
    /// read as the program not being found.
    fn command(&self, way: Way) -> Result<Command, SpawnCause> {
        if let Some(dir) = &self.cwd {
            check_dir(dir)?;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "sb-exec is where every child process starts (CONSTRAINTS.md E4)"
        )]
        let mut command = Command::new(&self.program);
        for arg in &self.args {
            command.arg(arg.text());
        }
        let environment = self.environment(way);
        if environment.clear {
            command.env_clear();
        }
        for (name, value) in environment.vars {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }
        if way == Way::Attached {
            // sb's own stdio and group (the terminal's signals reach it).
            command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .kill_on_drop(true);
            return Ok(command);
        }
        let stdin = if self.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };
        command
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        Ok(command)
    }
}

/// Read `pipe` line by line to its end, each line (without its `\n`, cut at
/// [`LINE`] with the rest dropped up to its end) masked and sent to
/// `lines`; with `tail`, the masked lines are also kept, the last
/// [`STDERR_TAIL`] bytes of them. A final line without `\n` is sent too.
/// `line` holds the line being read, empty between lines: tokio's
/// `read_until` appends what it has read to the buffer it was given, so
/// if this future is dropped mid-line (the drain's end) the caller still
/// has the start of the line
/// (<https://docs.rs/tokio/1.54.1/tokio/io/trait.AsyncBufReadExt.html#cancel-safety>),
/// and a read error mid-line leaves it there too. Why not tokio's
/// `Lines::next_line`: its partial line stays in its own buffer, out of
/// reach once dropped, and it takes UTF-8 only.
async fn read_lines(
    pipe: Option<impl AsyncRead + Unpin>,
    mask: &Mask,
    lines: mpsc::Sender<Vec<u8>>,
    mut tail: Option<&mut Kept>,
    line: &mut Vec<u8>,
) {
    let Some(pipe) = pipe else { return };
    let mut reader = BufReader::new(pipe);
    // The window reaches the longest secret past the cut, so a secret
    // across it is still found whole (as capture's caps).
    let window = LINE + mask.longest();
    let limit = u64::try_from(window + 1).unwrap_or(u64::MAX);
    loop {
        match (&mut reader).take(limit).read_until(b'\n', line).await {
            Ok(1..) => {}
            Ok(0) | Err(_) => return,
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        } else if line.len() > window && !skip_line(&mut reader).await {
            // The pipe ended inside the dropped rest: this line is the last.
            send_line(line, mask, &lines, tail.as_deref_mut()).await;
            return;
        }
        if !send_line(line, mask, &lines, tail.as_deref_mut()).await {
            return;
        }
    }
}

/// Mask `line`, keep it in `tail` if given, send it and clear it; false
/// once no one takes lines any more. The channel's slot is reserved
/// first, and the rest happens at once after it, so a reader dropped
/// while it waits (the drain's end) has kept nothing and its caller keeps
/// the line once: tokio's `Sender::reserve` is cancel safe and
/// `Permit::send` doesn't wait
/// (<https://docs.rs/tokio/1.54.1/tokio/sync/mpsc/struct.Sender.html#method.reserve>).
async fn send_line(
    line: &mut Vec<u8>,
    mask: &Mask,
    lines: &mpsc::Sender<Vec<u8>>,
    tail: Option<&mut Kept>,
) -> bool {
    let Ok(slot) = lines.reserve().await else {
        line.clear();
        return false;
    };
    slot.send(keep_line(line, mask, tail));
    line.clear();
    true
}

/// `line` as the step gets it ([`shown_line`]), also kept in `tail` if
/// given.
fn keep_line(line: &[u8], mask: &Mask, tail: Option<&mut Kept>) -> Vec<u8> {
    let shown = shown_line(line, mask);
    if let Some(tail) = tail {
        tail.bytes.extend(&shown);
        tail.bytes.push_back(b'\n');
        let over = tail.bytes.len().saturating_sub(STDERR_TAIL);
        // Whole lines only, so the caller's match never sees half of one:
        // a cut inside a line drops that line's rest too.
        let inside_a_line = over > 0 && tail.bytes[over - 1] != b'\n';
        tail.bytes.drain(..over);
        if inside_a_line && let Some(end) = tail.bytes.iter().position(|&b| b == b'\n') {
            tail.bytes.drain(..=end);
        }
    }
    shown
}

/// A line as the step gets it: cut at [`LINE`], at a character's start
/// (a character split by the cut is dropped rather than shown as U+FFFD),
/// secrets taken out over the whole window read.
pub(crate) fn shown_line(line: &[u8], mask: &Mask) -> Vec<u8> {
    let mut cut = line.len().min(LINE);
    while cut > 0 && cut < line.len() && line[cut] & 0b1100_0000 == 0b1000_0000 {
        cut -= 1;
    }
    mask.bytes(line, 0..cut)
}

/// Drop the rest of a line past its cut, up to and with its `\n`; false if
/// the pipe ended first.
async fn skip_line(reader: &mut BufReader<impl AsyncRead + Unpin>) -> bool {
    let mut rest = Vec::new();
    let limit = u64::try_from(CHUNK).unwrap_or(u64::MAX);
    loop {
        rest.clear();
        match (&mut *reader)
            .take(limit)
            .read_until(b'\n', &mut rest)
            .await
        {
            Ok(1..) if rest.last() == Some(&b'\n') => return true,
            Ok(1..) => {}
            Ok(0) | Err(_) => return false,
        }
    }
}

/// `dir` is an existing directory, or why it can't be the child's.
pub(crate) fn check_dir(dir: &std::path::Path) -> Result<(), SpawnCause> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(SpawnCause::NoDirectory(dir.to_path_buf())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Err(SpawnCause::NoDirectory(dir.to_path_buf()))
        }
        Err(cause) => Err(SpawnCause::Directory {
            dir: dir.to_path_buf(),
            cause,
        }),
    }
}

/// Write `bytes` to the child's stdin from their own future, then close
/// it. A child that exits without reading them all is fine (EPIPE).
async fn write_stdin(pipe: Option<tokio::process::ChildStdin>, bytes: Option<&[u8]>) {
    if let (Some(mut pipe), Some(bytes)) = (pipe, bytes) {
        let _ = pipe.write_all(bytes).await;
    }
}

/// Read `pipe` to its end, keeping its first `keep` bytes. Reading goes on
/// past them: a full pipe would block the child. Why not `take(keep)` and
/// `read_to_end`, then `io::copy` to `io::sink()`: the drain's timeout
/// drops this future part-way, and of tokio's reads only `read` is
/// documented cancel safe
/// (<https://docs.rs/tokio/1.54.1/tokio/io/trait.AsyncReadExt.html#method.read>),
/// so what a dropped `read_to_end` leaves in the buffer isn't promised.
async fn read_head(pipe: Option<impl AsyncRead + Unpin>, keep: usize, kept: &mut Kept) {
    let Some(mut pipe) = pipe else { return };
    let mut chunk = vec![0; CHUNK];
    while let Ok(read @ 1..) = pipe.read(&mut chunk).await {
        let room = keep.saturating_sub(kept.bytes.len());
        let taken = read.min(room);
        kept.bytes.extend(&chunk[..taken]);
        kept.cut |= taken < read;
    }
}

/// Read `pipe` to its end, keeping its last `keep` bytes.
async fn read_tail(pipe: Option<impl AsyncRead + Unpin>, keep: usize, kept: &mut Kept) {
    let Some(mut pipe) = pipe else { return };
    let mut chunk = vec![0; CHUNK];
    while let Ok(read @ 1..) = pipe.read(&mut chunk).await {
        kept.bytes.extend(&chunk[..read]);
        let over = kept.bytes.len().saturating_sub(keep);
        kept.bytes.drain(..over);
        kept.cut |= over > 0;
    }
}

/// stdout's first 1 MiB as text, secrets taken out over the window
/// [`read_head`] kept: it reaches the longest secret past the cut.
fn head_text(mask: &Mask, window: &[u8]) -> String {
    let kept = 0..window.len().min(MAX_FILE_BYTES);
    String::from_utf8_lossy(&mask.bytes(window, kept)).into_owned()
}

/// stderr's last 1 MiB as text, secrets taken out over the window
/// [`read_tail`] kept: it reaches the longest secret before the cut.
fn tail_text(mask: &Mask, window: &[u8]) -> String {
    let kept = window.len().saturating_sub(MAX_FILE_BYTES)..window.len();
    String::from_utf8_lossy(&mask.bytes(window, kept)).into_owned()
}

/// A code when the child exited, else the signal that killed it (`wait`
/// never reports a stopped or continued child, the only status with
/// neither).
pub(crate) fn ending(status: ExitStatus) -> Ending {
    status.code().map_or_else(
        || Ending::Signalled(status.signal().unwrap_or_default()),
        Ending::Exited,
    )
}

/// The run-log record of a child ending (spec, "Logging").
pub(crate) fn log_end(command: &str, outcome: &Outcome) {
    tracing::info!(
        command,
        outcome = %outcome.ending,
        elapsed_ms = crate::millis(outcome.elapsed),
        "child ended"
    );
}
