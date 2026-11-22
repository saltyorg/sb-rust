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
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
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
    let status = Command::new("sh")
        .args(["-c", "cp \"$1\" \"$2\" && chmod 755 \"$2\"", "sh"])
        .arg(from)
        .arg(to)
        .status()
        .expect("start sh to copy a program");
    assert!(
        status.success(),
        "copying {} to {}: {status}",
        from.display(),
        to.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

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
        let ran = Command::new(&path).output().expect("run");
        assert_eq!(ran.stdout, b"ran\n");
        // Written again in place: the new text, still executable.
        write_program(&path, b"#!/bin/sh\necho 'again'\n");
        assert_eq!(
            Command::new(&path).output().expect("run").stdout,
            b"again\n"
        );
    }

    #[test]
    fn a_copied_program_runs() {
        let dir = tempfile::tempdir().expect("dir");
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        write_program(&from, "#!/bin/sh\necho 'copied'\n");
        copy_program(&from, &to);
        let mode = std::fs::metadata(&to).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(Command::new(&to).output().expect("run").stdout, b"copied\n");
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
}
