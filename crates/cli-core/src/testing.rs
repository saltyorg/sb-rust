//! Support for the crates' tests (the `testing` feature, never in a
//! release build).

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// Writes `contents` to `path`, mode 0755, for a test to run as a
/// stand-in program.
///
/// A child (`sh`, fed on stdin) writes it, so no thread of the test
/// binary ever holds a write descriptor to it. One that did would leak
/// it into any child another test forks meanwhile (`O_CLOEXEC` closes it
/// only at that child's exec), and running the program then fails with
/// ETXTBSY: exec refuses a file that is open for writing
/// (<https://man7.org/linux/man-pages/man2/execve.2.html>, ETXTBSY). std
/// has no fix; writing the file from a child is a workaround its issue
/// lists (<https://github.com/rust-lang/rust/issues/114554>).
///
/// Why not write a temporary name, close it and rename it into place (as
/// tempfile's `NamedTempFile::persist` does,
/// <https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist>):
/// the rename keeps the inode, which the other child still holds open for
/// writing, so exec fails just the same. Measured: 2,000 runs of a
/// freshly written script beside three threads spawning `true`, on four
/// cores, failed with ETXTBSY 80 times written in place, 96 times renamed
/// into place, and never written by a child. A lock held around every
/// write and spawn (the `SPAWNS` mutexes) helps only while every test in
/// the binary that spawns anything takes it.
///
/// # Panics
///
/// When `sh` can't be started or the file can't be written.
#[track_caller]
pub fn write_program(path: &Path, contents: impl AsRef<[u8]>) {
    // Started under the fork lock, as sb-exec starts children, so it
    // can't inherit a program sb's code is writing in another test.
    let mut child = crate::fork_lock::forking(|| {
        Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
    })
    .expect("start sh to write a stand-in program");
    // The handle is dropped at the end of the statement, closing the pipe:
    // cat sees its end. A shell that failed first (a missing directory)
    // has closed it, and the write's EPIPE is reported with its status.
    let fed = child
        .stdin
        .take()
        .expect("sh's stdin")
        .write_all(contents.as_ref());
    let status = child.wait().expect("wait for sh");
    assert!(
        status.success() && fed.is_ok(),
        "writing {}: {status}, fed: {fed:?}",
        path.display()
    );
}

/// Copies the program at `from` to `to`, mode 0755, by a child (`cp`),
/// for the reason [`write_program`] gives: a copy made in this process
/// would hold `to` open for writing as long as the copy takes.
///
/// # Panics
///
/// When `sh` can't be started or the copy fails.
#[track_caller]
pub fn copy_program(from: &Path, to: &Path) {
    // Started under the fork lock, as `write_program` is.
    let status = crate::fork_lock::forking(|| {
        Command::new("sh")
            .args(["-c", "cp \"$1\" \"$2\" && chmod 755 \"$2\"", "sh"])
            .arg(from)
            .arg(to)
            .spawn()
    })
    .and_then(|mut child| child.wait())
    .expect("start sh to copy a program");
    assert!(
        status.success(),
        "copying {} to {}: {status}",
        from.display(),
        to.display()
    );
}

/// A child start held in progress: cli-core's fork lock taken shared, as
/// sb-exec takes it around a spawn, on a thread of its own until this is
/// dropped. Code that writes a program sb runs must wait for it
/// ([`crate::fork_lock`]).
#[derive(Debug)]
pub struct ForkHeld {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Hold a child start in progress until the result is dropped.
///
/// # Panics
///
/// When the holding thread can't start or dies before it holds.
#[must_use]
pub fn fork_held() -> ForkHeld {
    let (holding, held) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        crate::fork_lock::forking(|| {
            holding.send(()).expect("holding");
            // Until the sender is dropped.
            let _ = released.recv();
        });
    });
    held.recv().expect("the fork lock held");
    ForkHeld {
        release: Some(release),
        thread: Some(thread),
    }
}

impl Drop for ForkHeld {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `work` on a thread of its own while a child start is held: it must
/// not have finished a while later (100 ms, long enough for work that
/// isn't held back), and finishes once the hold ends. Its result.
///
/// # Panics
///
/// When `work` finished while the child start was held, or panicked.
#[track_caller]
pub fn waits_for_a_fork<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let held = fork_held();
    let thread = std::thread::spawn(work);
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(!thread.is_finished(), "finished while a child was starting");
    drop(held);
    thread.join().expect("work")
}

/// A child's stdout and stderr in files, for the test's failure messages.
///
/// The files are `<name>.stdout` and `<name>.stderr` in a test's temporary
/// directory; `Display` shows them, so a child that fails says why rather
/// than leaving its exit status alone.
///
/// A file, never a pipe the test doesn't read: a full pipe blocks the
/// child's next write (64 KiB on Linux, pipe(7),
/// <https://man7.org/linux/man-pages/man7/pipe.7.html>), and a child
/// whose ending the test waits for would hang it. The file is std's
/// `Stdio` from a `File`
/// (<https://doc.rust-lang.org/std/process/struct.Stdio.html#impl-From%3CFile%3E-for-Stdio>).
/// Why not `assert_cmd`, which the tests use, or `Command::output`: both
/// capture a child run to its end, and these are children the test signals
/// or watches while they run
/// (<https://docs.rs/assert_cmd/2.2.2/assert_cmd/cmd/struct.Command.html>).
/// Nor duct's `stdout_path` and `stderr_path`
/// (<https://docs.rs/duct/1.1.2/duct/struct.Expression.html>), a new
/// dependency that would wrap each test's `std::process::Command` in its
/// own expression type for what these few lines do.
#[derive(Debug, Clone)]
pub struct ChildOutput {
    name: String,
    stdout: std::path::PathBuf,
    stderr: std::path::PathBuf,
}

impl ChildOutput {
    /// The files for the child `name` in `dir`; none is made yet.
    #[must_use]
    pub fn new(dir: &Path, name: &str) -> Self {
        Self {
            name: name.to_owned(),
            stdout: dir.join(format!("{name}.stdout")),
            stderr: dir.join(format!("{name}.stderr")),
        }
    }

    /// Both streams of `cmd` to the files.
    ///
    /// # Panics
    ///
    /// When a file can't be made.
    #[track_caller]
    pub fn attach<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        cmd.stdout(self.stdout()).stderr(self.stderr())
    }

    /// The stdout file, for `Command::stdout`: each call makes it empty
    /// again, so it is set once for each child.
    ///
    /// # Panics
    ///
    /// When it can't be made.
    #[track_caller]
    #[must_use]
    pub fn stdout(&self) -> Stdio {
        Self::create(&self.stdout)
    }

    /// The stderr file, for `Command::stderr`: each call makes it empty
    /// again, so it is set once for each child.
    ///
    /// # Panics
    ///
    /// When it can't be made.
    #[track_caller]
    #[must_use]
    pub fn stderr(&self) -> Stdio {
        Self::create(&self.stderr)
    }

    /// What the child wrote to stdout so far, lossily UTF-8 (empty if none).
    #[must_use]
    pub fn stdout_text(&self) -> String {
        Self::read(&self.stdout).unwrap_or_default()
    }

    /// What the child wrote to stderr so far, lossily UTF-8 (empty if none).
    #[must_use]
    pub fn stderr_text(&self) -> String {
        Self::read(&self.stderr).unwrap_or_default()
    }

    #[track_caller]
    fn create(path: &Path) -> Stdio {
        std::fs::File::create(path)
            .unwrap_or_else(|e| panic!("making {}: {e}", path.display()))
            .into()
    }

    /// The file's text, or `None` when there is no such file.
    fn read(path: &Path) -> Option<String> {
        std::fs::read(path)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Each captured stream, labelled with the child's name, its text ending
/// in a newline: `<name>'s stdout:` and the text, or `(empty)`.
impl std::fmt::Display for ChildOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut shown = false;
        for (stream, path) in [("stdout", &self.stdout), ("stderr", &self.stderr)] {
            let Some(text) = Self::read(path) else {
                continue;
            };
            shown = true;
            if text.is_empty() {
                writeln!(f, "{}'s {stream}: (empty)", self.name)?;
            } else {
                let end = if text.ends_with('\n') { "" } else { "\n" };
                write!(f, "{}'s {stream}:\n{text}{end}", self.name)?;
            }
        }
        if !shown {
            writeln!(f, "{}: no output captured", self.name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// The program at `path` run to its end, started under the fork lock
    /// as every child of cli-core's unit tests is: the run lock's tests
    /// keep its descriptor out of them that way.
    fn run(path: &Path) -> std::process::Output {
        crate::fork_lock::forking(|| Command::new(path).output()).expect("run")
    }

    #[test]
    fn a_program_is_written_whole_and_executable() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("program");
        write_program(&path, "#!/bin/sh\necho 'ran'\n");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "#!/bin/sh\necho 'ran'\n"
        );
        let ran = run(&path);
        assert_eq!(ran.stdout, b"ran\n");
        // Written again in place: the new text, still executable.
        write_program(&path, b"#!/bin/sh\necho 'again'\n");
        assert_eq!(run(&path).stdout, b"again\n");
    }

    #[test]
    fn a_copied_program_runs() {
        let dir = tempfile::tempdir().expect("dir");
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        write_program(&from, "#!/bin/sh\necho 'copied'\n");
        copy_program(&from, &to);
        let mode = std::fs::metadata(&to).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(run(&to).stdout, b"copied\n");
    }

    #[test]
    #[should_panic(expected = "copying ")]
    fn a_missing_program_cant_be_copied() {
        let dir = tempfile::tempdir().expect("dir");
        copy_program(&dir.path().join("absent"), &dir.path().join("to"));
    }

    #[test]
    #[should_panic(expected = "writing ")]
    fn a_program_that_cant_be_written_panics_with_its_path() {
        let dir = tempfile::tempdir().expect("dir");
        write_program(&dir.path().join("no-such-dir/program"), "#!/bin/sh\n");
    }

    /// `script` run by `sh` with its output in `output`'s files, to its
    /// end.
    fn ran(script: &str, output: impl FnOnce(&mut Command)) -> std::process::ExitStatus {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        output(&mut cmd);
        crate::fork_lock::forking(|| cmd.spawn())
            .and_then(|mut child| child.wait())
            .expect("run sh")
    }

    #[test]
    fn a_childs_output_is_shown_by_stream() {
        let dir = tempfile::tempdir().expect("dir");
        let output = ChildOutput::new(dir.path(), "sh");
        let status = ran("echo out; echo err >&2; printf 'no newline' >&2", |cmd| {
            output.attach(cmd);
        });
        assert!(status.success(), "{status}");
        assert_eq!(output.stdout_text(), "out\n");
        assert_eq!(output.stderr_text(), "err\nno newline");
        assert_eq!(
            output.to_string(),
            "sh's stdout:\nout\nsh's stderr:\nerr\nno newline\n"
        );
    }

    #[test]
    fn only_the_streams_captured_are_shown() {
        let dir = tempfile::tempdir().expect("dir");
        let output = ChildOutput::new(dir.path(), "sh");
        let status = ran("echo out; echo err >&2", |cmd| {
            cmd.stdout(Stdio::piped()).stderr(output.stderr());
        });
        assert!(status.success(), "{status}");
        assert_eq!(output.to_string(), "sh's stderr:\nerr\n");
        // A stream written to nothing yet, or never started, says so.
        let silent = ChildOutput::new(dir.path(), "quiet");
        let status = ran("true", |cmd| {
            silent.attach(cmd);
        });
        assert!(status.success(), "{status}");
        assert_eq!(
            silent.to_string(),
            "quiet's stdout: (empty)\nquiet's stderr: (empty)\n"
        );
        let never = ChildOutput::new(dir.path(), "never");
        assert_eq!(never.to_string(), "never: no output captured\n");
    }

    /// A child that writes far more than a pipe holds (64 KiB on Linux,
    /// pipe(7)) ends with nothing reading it: a file never fills.
    #[test]
    fn a_child_that_writes_much_never_blocks() {
        let dir = tempfile::tempdir().expect("dir");
        let output = ChildOutput::new(dir.path(), "sh");
        let status = ran(
            "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
            |cmd| {
                output.attach(cmd);
            },
        );
        assert!(status.success(), "{status}");
        assert_eq!(output.stdout_text().len(), 1_048_576);
        assert_eq!(output.stderr_text().len(), 1_048_576);
    }

    #[test]
    fn output_that_isnt_utf8_is_shown_lossily() {
        let dir = tempfile::tempdir().expect("dir");
        let output = ChildOutput::new(dir.path(), "sh");
        let status = ran(r"printf 'a\377b' >&2", |cmd| {
            output.attach(cmd);
        });
        assert!(status.success(), "{status}");
        assert_eq!(output.stderr_text(), "a\u{fffd}b");
    }

    #[test]
    #[should_panic(expected = "no-such-dir")]
    fn an_output_file_that_cant_be_made_panics_with_its_path() {
        let dir = tempfile::tempdir().expect("dir");
        let _ = ChildOutput::new(&dir.path().join("no-such-dir"), "sh").stdout();
    }
}
