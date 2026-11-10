//! Opening journal files safely (security audit L2), and where the local
//! journal is (`Host`).
//!
//! A journal file is opened read-only with `O_NONBLOCK|O_NOCTTY|O_CLOEXEC`
//! and `O_NOFOLLOW`, then checked with `fstat` to be a regular file:
//! - `O_NONBLOCK`: opening a FIFO put where a journal file was doesn't
//!   wait for a writer; the `fstat` check then refuses it, so no read can
//!   block either;
//! - `O_NOCTTY`: a terminal device can't become sb's controlling terminal;
//! - `O_CLOEXEC`: no descriptor leaks into a child (std sets it too);
//! - `O_NOFOLLOW`: a journal file that is a symlink isn't followed, and is
//!   reported (`ReadErrorKind::Symlink`), never dropped silently. sd-journal
//!   follows it (journal spec, Difference 4). It applies to the last
//!   component only, so a symlinked directory (a `/var/log` on another
//!   disk) is still read.
//!
//! The flags go through std's `OpenOptionsExt::custom_flags`, as its own
//! example passes `O_NOFOLLOW`
//! (<https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html#tymethod.custom_flags>),
//! with the values from rustix's `OFlags`
//! (<https://docs.rs/rustix/1.1.6/rustix/fs/struct.OFlags.html>), so no
//! `libc` dependency is added. The check is `File::metadata`, which is
//! `fstat` on the open descriptor, so it checks the file that was opened
//! (<https://doc.rust-lang.org/std/fs/struct.File.html#method.metadata>).
//!
//! What this doesn't cover, accepted:
//! - the `fstat` check comes after the open, so a device node named as a
//!   journal file is opened before it is refused, and opening some devices
//!   has effects of its own. Only `Source::Files`, or a directory of the
//!   path that someone else controls, can name one: a listing takes only
//!   regular files and symlinks (`walk::scan`), and the journal roots are
//!   root's. Why not `O_PATH`, `fstat`, then reopening through
//!   `/proc/self/fd`: it needs /proc, and a third system call per file,
//!   for a case sb's sources don't reach;
//! - `O_NONBLOCK` does nothing for a regular file: a `pread` on a hung
//!   network or FUSE filesystem can still block.
//!
//! Each `Journal`'s `Opener` counts its opens, closes and preads, per
//! journal and not in a static (M-AVOID-STATICS), so tests can bound a
//! page's reads and see every file closed. The counts are read only under
//! the `testing` feature (`testing::counts`); keeping them is a relaxed
//! atomic add beside each system call, so they aren't compiled out (no
//! `cfg`'d fields).

use crate::ReadErrorKind;
use rustix::fs::OFlags;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Where `Source::Local` finds the local journal: the machine-id file and
/// the journal roots, in sd-journal's search order. `Host::default()` is
/// the system's; tests inject their own (`testing::open_on`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The machine-id file, `/etc/machine-id`.
    pub machine_id: PathBuf,
    /// The roots each joined with the machine id: `/run/log/journal`,
    /// then `/var/log/journal`.
    pub roots: [PathBuf; 2],
}

impl Default for Host {
    fn default() -> Self {
        Self {
            machine_id: PathBuf::from("/etc/machine-id"),
            roots: [
                PathBuf::from("/run/log/journal"),
                PathBuf::from("/var/log/journal"),
            ],
        }
    }
}

/// The most bytes read from a machine-id file: 32 digits and a newline
/// fit, so a larger file is refused without reading it all.
const MACHINE_ID_READ_MAX: u64 = 64;

/// The machine-id file's contents, at most `MACHINE_ID_READ_MAX` bytes,
/// opened as a journal file is but following a symlink: a read-only root
/// may link `/etc/machine-id` to a file under `/run`.
///
/// # Errors
///
/// The system's, or `InvalidData` for a file that isn't a regular file.
pub(crate) fn read_machine_id(path: &Path) -> io::Result<Vec<u8>> {
    let (file, _) = open_regular(path, OFlags::empty())?;
    let mut out = Vec::new();
    file.take(MACHINE_ID_READ_MAX).read_to_end(&mut out)?;
    Ok(out)
}

/// Why a regular file couldn't be opened.
#[derive(Debug)]
enum Refused {
    /// The system refused the open.
    Open(io::Error),
    /// The system refused the `fstat`.
    Fstat(io::Error),
    /// The file is something other than a regular file.
    NotRegular,
}

impl From<Refused> for io::Error {
    fn from(r: Refused) -> Self {
        match r {
            Refused::Open(e) | Refused::Fstat(e) => e,
            Refused::NotRegular => io::Error::new(io::ErrorKind::InvalidData, "not a regular file"),
        }
    }
}

/// Open `path` read-only with the module's flags plus `extra`, and check
/// that it is a regular file; the file and its `fstat`.
fn open_regular(path: &Path, extra: OFlags) -> Result<(File, Metadata), Refused> {
    let flags = OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC | extra;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits().cast_signed())
        .open(path)
        .map_err(Refused::Open)?;
    let meta = file.metadata().map_err(Refused::Fstat)?;
    if meta.file_type().is_file() {
        Ok((file, meta))
    } else {
        Err(Refused::NotRegular)
    }
}

/// Opens a journal's files, counting for the one `Journal` holding it the
/// files opened and closed and the preads made.
#[derive(Debug, Clone, Default)]
pub(crate) struct Opener {
    counts: Arc<Counters>,
}

#[derive(Debug, Default)]
struct Counters {
    opens: AtomicU64,
    closes: AtomicU64,
    preads: AtomicU64,
}

/// What a `Journal`'s opener counted since the journal was opened
/// (`testing::counts`).
#[cfg(feature = "testing")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Journal files opened.
    pub opens: u64,
    /// Journal files closed.
    pub closes: u64,
    /// `pread` calls made on them.
    pub preads: u64,
}

impl Opener {
    /// Open the journal file at `path` (the module's flags, `O_NOFOLLOW`
    /// included).
    ///
    /// # Errors
    ///
    /// `ReadErrorKind::Symlink` for a symlink, `NotRegular` for anything
    /// else not a regular file, else `Io` with the system's error.
    pub(crate) fn open(&self, path: &Path) -> Result<Handle, ReadErrorKind> {
        let system = |context: &str, source| ReadErrorKind::Io {
            context: context.to_owned(),
            source,
        };
        match open_regular(path, OFlags::NOFOLLOW) {
            Ok((file, meta)) => {
                self.counts.opens.fetch_add(1, Ordering::Relaxed);
                Ok(Handle {
                    file,
                    inode: (meta.dev(), meta.ino()),
                    counts: Arc::clone(&self.counts),
                })
            }
            Err(Refused::NotRegular) => Err(ReadErrorKind::NotRegular),
            // ELOOP is also a symlink loop in a directory of the path: only
            // a symlink as the last component is this one.
            // Why not `io::ErrorKind::FilesystemLoop`: it is unstable
            // (`io_error_more`); rustix's typed errno is
            // (<https://docs.rs/rustix/1.1.6/rustix/io/struct.Errno.html#method.from_io_error>).
            Err(Refused::Open(e))
                if rustix::io::Errno::from_io_error(&e) == Some(rustix::io::Errno::LOOP)
                    && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) =>
            {
                Err(ReadErrorKind::Symlink)
            }
            Err(Refused::Open(e)) => Err(system("open", e)),
            Err(Refused::Fstat(e)) => Err(system("metadata", e)),
        }
    }

    /// The counts so far.
    #[cfg(feature = "testing")]
    pub(crate) fn counts(&self) -> Counts {
        Counts {
            opens: self.counts.opens.load(Ordering::Relaxed),
            closes: self.counts.closes.load(Ordering::Relaxed),
            preads: self.counts.preads.load(Ordering::Relaxed),
        }
    }
}

/// An open journal file: preads and `fstat` only.
#[derive(Debug)]
pub(crate) struct Handle {
    file: File,
    /// The device and inode from the open's `fstat`.
    inode: (u64, u64),
    counts: Arc<Counters>,
}

impl Handle {
    /// The open file's device and inode.
    pub(crate) fn inode(&self) -> (u64, u64) {
        self.inode
    }

    /// One `pread` (`FileExt::read_at`).
    pub(crate) fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.counts.preads.fetch_add(1, Ordering::Relaxed);
        self.file.read_at(buf, offset)
    }

    /// `FileExt::read_exact_at`, counted as one pread: it repeats only
    /// after a short read, which a regular file gives only at its end,
    /// where the read fails.
    pub(crate) fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        self.counts.preads.fetch_add(1, Ordering::Relaxed);
        self.file.read_exact_at(buf, offset)
    }

    /// `fstat`.
    pub(crate) fn metadata(&self) -> io::Result<Metadata> {
        self.file.metadata()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.counts.closes.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn kind(r: Result<Handle, ReadErrorKind>) -> ReadErrorKind {
        r.expect_err("refused")
    }

    /// A FIFO where a journal file was: the open doesn't wait for a writer
    /// (the test would hang), and the FIFO is refused as not a regular
    /// file.
    #[test]
    fn a_fifo_does_not_block_and_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let fifo = dir.path().join("system.journal");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RUSR).expect("mkfifo");
        let opener = Opener::default();
        assert!(matches!(
            kind(opener.open(&fifo)),
            ReadErrorKind::NotRegular
        ));
        // Refused before it became a journal file's handle: not counted.
        assert_eq!((opener.counts().opens, opener.counts().closes), (0, 0));
        let err: io::Error = open_regular(&fifo, OFlags::empty())
            .expect_err("refused")
            .into();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(err.to_string(), "not a regular file");
    }

    /// A symlinked journal file isn't followed: its own error (Difference
    /// 4), while the file it names opens.
    #[test]
    fn a_symlinked_file_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let target = dir.path().join("real.journal");
        std::fs::write(&target, b"x").expect("write");
        let link = dir.path().join("link.journal");
        symlink(&target, &link).expect("link");
        let opener = Opener::default();
        assert!(matches!(kind(opener.open(&link)), ReadErrorKind::Symlink));
        assert!(opener.open(&target).is_ok());
        // A dangling one is refused the same way.
        let dangling = dir.path().join("dangling.journal");
        symlink(dir.path().join("gone"), &dangling).expect("link");
        assert!(matches!(
            kind(opener.open(&dangling)),
            ReadErrorKind::Symlink
        ));
    }

    /// A symlinked directory is followed: a file under it opens.
    #[test]
    fn a_symlinked_directory_is_followed() {
        let dir = tempfile::tempdir().expect("dir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("dir");
        std::fs::write(real.join("system.journal"), b"x").expect("write");
        symlink(&real, dir.path().join("linked")).expect("link");
        let opener = Opener::default();
        assert!(
            opener
                .open(&dir.path().join("linked/system.journal"))
                .is_ok()
        );
    }

    /// ELOOP from a loop of directory symlinks in the path isn't taken for
    /// a symlinked file: it is the system's error.
    #[test]
    fn a_loop_in_the_directories_is_the_systems_error() {
        let dir = tempfile::tempdir().expect("dir");
        symlink(dir.path().join("b"), dir.path().join("a")).expect("link");
        symlink(dir.path().join("a"), dir.path().join("b")).expect("link");
        let err = kind(Opener::default().open(&dir.path().join("a/system.journal")));
        let ReadErrorKind::Io { context, source } = err else {
            panic!("{err:?}");
        };
        assert_eq!(context, "open");
        assert_eq!(
            source.raw_os_error(),
            Some(rustix::io::Errno::LOOP.raw_os_error())
        );
    }

    /// A missing file and a directory: the system's errors, named by what
    /// failed.
    #[test]
    fn other_failures_are_the_systems_errors() {
        let dir = tempfile::tempdir().expect("dir");
        let err = kind(Opener::default().open(&dir.path().join("gone.journal")));
        assert!(
            matches!(&err, ReadErrorKind::Io { context, source } if context == "open" && source.kind() == io::ErrorKind::NotFound),
            "{err:?}"
        );
        // A directory opens read-only, and fstat refuses it.
        assert!(matches!(
            kind(Opener::default().open(dir.path())),
            ReadErrorKind::NotRegular
        ));
        for refused in [
            Refused::Open(io::Error::other("x")),
            Refused::Fstat(io::Error::other("x")),
        ] {
            assert_eq!(io::Error::from(refused).to_string(), "x");
        }
    }

    /// Every pread is counted, and a file is counted closed when its
    /// handle is dropped; each opener counts only its own files.
    #[test]
    fn the_opener_counts_opens_closes_and_preads() {
        let file = tempfile::NamedTempFile::new().expect("temp");
        std::fs::write(file.path(), b"0123456789").expect("write");
        let opener = Opener::default();
        let other = Opener::default();
        let h = opener.open(file.path()).expect("opens");
        let mut buf = [0u8; 4];
        assert_eq!(h.read_at(&mut buf, 2).expect("read"), 4);
        assert_eq!(&buf, b"2345");
        h.read_exact_at(&mut buf, 6).expect("read");
        assert_eq!(&buf, b"6789");
        assert!(h.read_exact_at(&mut buf, 8).is_err());
        let meta = h.metadata().expect("fstat");
        assert_eq!(meta.len(), 10);
        assert_eq!(h.inode(), (meta.dev(), meta.ino()));
        let shared = opener.clone();
        assert_eq!(
            shared.counts(),
            Counts {
                opens: 1,
                closes: 0,
                preads: 3
            }
        );
        drop(h);
        assert_eq!(opener.counts().closes, 1);
        assert_eq!(other.counts(), Counts::default());
    }

    /// The machine-id file: read through a symlink, at most 64 bytes, and
    /// refused when it isn't a regular file.
    #[test]
    fn the_machine_id_file_is_read_bounded() {
        let dir = tempfile::tempdir().expect("dir");
        let id = dir.path().join("machine-id");
        std::fs::write(&id, "a".repeat(100)).expect("write");
        assert_eq!(read_machine_id(&id).expect("reads").len(), 64);
        let link = dir.path().join("link");
        symlink(&id, &link).expect("link");
        assert_eq!(read_machine_id(&link).expect("reads").len(), 64);
        let err = read_machine_id(dir.path()).expect_err("a directory");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            read_machine_id(&dir.path().join("gone"))
                .expect_err("missing")
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn the_default_host_is_the_systems() {
        let host = Host::default();
        assert_eq!(host.machine_id, Path::new("/etc/machine-id"));
        assert_eq!(
            host.roots,
            [
                PathBuf::from("/run/log/journal"),
                PathBuf::from("/var/log/journal")
            ]
        );
    }
}
