//! Piped children through `tokio::process` (spec, "Ways to run"):
//! `capture` so far.

use std::collections::VecDeque;
use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use sb_cli_core::config::MAX_FILE_BYTES;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::mask::Mask;
use crate::{Captured, Cmd, Ending, Outcome, SpawnCause, SpawnError, Way};

/// How long output is still read after the child exits: a background
/// grandchild can hold the pipe open (spec, "After the child exits"; sb-go
/// handles it with `WaitDelay`).
const DRAIN: Duration = Duration::from_secs(2);

/// Bytes read from a pipe at a time.
const CHUNK: usize = 8 * 1024;

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
    /// background grandchild can put off for good. The token acts with the
    /// process-group handling (exec plan, Task 4).
    pub async fn capture(&self, _token: &CancellationToken) -> Result<Captured, SpawnError> {
        let (command, _) = self.shown();
        let failed = |cause: SpawnCause| SpawnError {
            program: self.program.to_string_lossy().into_owned(),
            cause,
        };
        let spawned = self
            .command(Way::Capture)
            .and_then(|mut child| child.spawn().map_err(SpawnCause::from));
        let mut child = match spawned {
            Ok(child) => child,
            Err(cause) => {
                // As an error value, so its causes are logged too
                // (`error.sources`, as cli-core's run record).
                let error: &(dyn std::error::Error + 'static) = &cause;
                tracing::warn!(command, error, "child not started");
                return Err(failed(cause));
            }
        };
        let start = Instant::now();
        self.log_start(Way::Capture);
        let mask = self.mask();
        let keep = MAX_FILE_BYTES + mask.longest();
        let (mut stdout, mut stderr) = (Kept::default(), Kept::default());
        let stdin = (child.stdin.take(), self.stdin.as_deref());
        let pipes = (child.stdout.take(), child.stderr.take());
        let status = {
            let io = async {
                tokio::join!(
                    write_stdin(stdin.0, stdin.1),
                    read_head(pipes.0, keep, &mut stdout),
                    read_tail(pipes.1, keep, &mut stderr),
                )
            };
            tokio::pin!(io);
            let mut drained = false;
            let status = tokio::select! {
                status = child.wait() => status,
                _ = &mut io => {
                    drained = true;
                    child.wait().await
                }
            };
            if !drained {
                let _ = tokio::time::timeout(DRAIN, &mut io).await;
            }
            status.map_err(|err| failed(err.into()))?
        };
        let outcome = Outcome {
            ending: ending(status),
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

    /// tokio's `Command` for `way`: program, arguments, the environment
    /// rules, working directory, piped output and its own process group
    /// (`process_group(0)`,
    /// https://docs.rs/tokio/1.54.1/tokio/process/struct.Command.html#method.process_group),
    /// killed if the run is dropped before it ends (`kill_on_drop`). The
    /// working directory is checked first: a missing one would otherwise
    /// read as the program not being found.
    fn command(&self, way: Way) -> Result<Command, SpawnCause> {
        if let Some(dir) = &self.cwd {
            match std::fs::metadata(dir) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => return Err(SpawnCause::NoDirectory(dir.clone())),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SpawnCause::NoDirectory(dir.clone()));
                }
                Err(cause) => {
                    return Err(SpawnCause::Directory {
                        dir: dir.clone(),
                        cause,
                    });
                }
            }
        }
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
/// (https://docs.rs/tokio/1.54.1/tokio/io/trait.AsyncReadExt.html#method.read),
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
fn ending(status: ExitStatus) -> Ending {
    status.code().map_or_else(
        || Ending::Signalled(status.signal().unwrap_or_default()),
        Ending::Exited,
    )
}

/// The run-log record of a child ending (spec, "Logging").
fn log_end(command: &str, outcome: &Outcome) {
    tracing::info!(
        command,
        outcome = %outcome.ending,
        elapsed_ms = u64::try_from(outcome.elapsed.as_millis()).unwrap_or(u64::MAX),
        "child ended"
    );
}
