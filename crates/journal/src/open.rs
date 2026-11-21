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

/// The size of a block [`Blocks`] reads with one `pread`.
pub(crate) const BLOCK: usize = 64 * 1024;

/// The blocks [`Blocks`] keeps, for all of a scan's files together.
const KEPT: usize = 4;

/// The last few blocks a whole-log scan read, shared by its files
/// (`Journal::scan`), newest first: a scan reads most of each file, an
/// object at a time, and four preads an entry were two thirds of its time
/// (perf round 3). Each block is one `pread` of [`BLOCK`] bytes, still no
/// mmap (ADR-013). One budget for the scan, whatever its number of files:
/// at most [`KEPT`] blocks are held. Only a sealed file's reads come here
/// (`JournalFile::read_in_blocks`), so a kept block never goes stale.
/// Pages and the follow read an object at a time: a page reads scattered
/// objects, where a block costs more than it saves. Why not std's
/// `BufReader`: it buffers one stream's position, where a scan reads
/// back and forth over several files under one budget; why not the `lru`
/// crate (in the tree through ratatui): for four entries a list is
/// lighter than its map.
#[derive(Debug, Default)]
pub(crate) struct Blocks {
    /// ((file, block number), the block's bytes; short at the file's end).
    kept: Vec<((usize, u64), Vec<u8>)>,
}

impl Blocks {
    /// `buf` filled from `offset` of `file`, the scan's file number
    /// `id`, through the kept blocks. A read larger than a block, past
    /// the file's end, or one whose block can't be read is a `pread` of
    /// its own, so its result and its error are what they would be
    /// without the blocks.
    pub(crate) fn read_exact_at(
        &mut self,
        file: &Handle,
        id: usize,
        buf: &mut [u8],
        offset: u64,
    ) -> io::Result<()> {
        if buf.len() <= BLOCK && self.fill(file, id, buf, offset) {
            return Ok(());
        }
        file.read_exact_at(buf, offset)
    }

    /// `buf` copied from the blocks it spans; `false` when one of them
    /// can't be read or ends before `buf` does.
    fn fill(&mut self, file: &Handle, id: usize, buf: &mut [u8], offset: u64) -> bool {
        let mut done = 0;
        while done < buf.len() {
            let Some(at) = offset.checked_add(done as u64) else {
                return false;
            };
            let size = BLOCK as u64;
            let Ok(within) = usize::try_from(at % size) else {
                return false;
            };
            let Some(block) = self.block(file, (id, at / size)) else {
                return false;
            };
            let n = (buf.len() - done).min(block.len().saturating_sub(within));
            if n == 0 {
                return false;
            }
            buf[done..done + n].copy_from_slice(&block[within..within + n]);
            done += n;
        }
        true
    }

    /// The block `key` names, moved to the front, read when it isn't
    /// kept (into the buffer of the block it replaces); `None` when the
    /// read fails.
    fn block(&mut self, file: &Handle, key: (usize, u64)) -> Option<&[u8]> {
        if let Some(i) = self.kept.iter().position(|(k, _)| *k == key) {
            self.kept[..=i].rotate_right(1);
        } else {
            let mut bytes = if self.kept.len() < KEPT {
                Vec::new()
            } else {
                self.kept.pop().map_or_default(|(_, bytes)| bytes)
            };
            bytes.resize(BLOCK, 0);
            // One pread: a regular file reads short only at its end, and
            // a read past a short block goes to the file itself.
            let have = file.read_at(&mut bytes, key.1 * BLOCK as u64).ok()?;
            bytes.truncate(have);
            self.kept.insert(0, (key, bytes));
        }
        self.kept.first().map(|(_, bytes)| bytes.as_slice())
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

    /// A file of `len` bytes, each its offset modulo 251, opened through
    /// `opener`.
    fn patterned(len: usize) -> (tempfile::NamedTempFile, Opener, Handle) {
        let file = tempfile::NamedTempFile::new().expect("temp");
        let bytes: Vec<u8> = (0..len)
            .map(|i| u8::try_from(i % 251).expect("small"))
            .collect();
        std::fs::write(file.path(), bytes).expect("write");
        let opener = Opener::default();
        let h = opener.open(file.path()).expect("opens");
        (file, opener, h)
    }

    fn expected(offset: usize, len: usize) -> Vec<u8> {
        (offset..offset + len)
            .map(|i| u8::try_from(i % 251).expect("small"))
            .collect()
    }

    /// Reads within a kept block cost no pread; one across two blocks
    /// reads both; the bytes are the file's wherever they lie.
    #[test]
    fn blocks_serve_reads_from_one_pread_a_block() {
        let (_file, opener, h) = patterned(3 * BLOCK);
        let mut blocks = Blocks::default();
        let mut buf = vec![0u8; 100];
        blocks.read_exact_at(&h, 0, &mut buf, 300).expect("reads");
        assert_eq!(buf, expected(300, 100));
        blocks.read_exact_at(&h, 0, &mut buf, 5_000).expect("reads");
        assert_eq!(buf, expected(5_000, 100));
        assert_eq!(opener.counts().preads, 1);
        let across = BLOCK - 40;
        blocks
            .read_exact_at(&h, 0, &mut buf, across as u64)
            .expect("reads");
        assert_eq!(buf, expected(across, 100));
        assert_eq!(opener.counts().preads, 2);
        // A whole block from its start, from what is kept.
        let mut whole = vec![0u8; BLOCK];
        blocks
            .read_exact_at(&h, 0, &mut whole, BLOCK as u64)
            .expect("reads");
        assert_eq!(whole, expected(BLOCK, BLOCK));
        assert_eq!(opener.counts().preads, 2);
    }

    /// A read larger than a block is one pread of its own, and keeps
    /// nothing.
    #[test]
    fn a_read_larger_than_a_block_goes_straight_to_the_file() {
        let (_file, opener, h) = patterned(3 * BLOCK);
        let mut blocks = Blocks::default();
        let mut buf = vec![0u8; BLOCK + 1];
        blocks.read_exact_at(&h, 0, &mut buf, 7).expect("reads");
        assert_eq!(buf, expected(7, BLOCK + 1));
        assert_eq!(opener.counts().preads, 1);
        assert_eq!(blocks.kept.len(), 0);
    }

    /// At the file's end the block is short: what it holds is served, a
    /// read past it fails as a direct read does, and an offset past the
    /// end fails the same.
    #[test]
    fn a_read_past_the_end_fails_as_a_direct_read_does() {
        let (_file, opener, h) = patterned(BLOCK + 10);
        let mut blocks = Blocks::default();
        let mut buf = vec![0u8; 8];
        blocks
            .read_exact_at(&h, 0, &mut buf, BLOCK as u64 + 2)
            .expect("reads");
        assert_eq!(buf, expected(BLOCK + 2, 8));
        let err = blocks
            .read_exact_at(&h, 0, &mut buf, BLOCK as u64 + 5)
            .expect_err("past the end");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        let err = blocks
            .read_exact_at(&h, 0, &mut buf, 3 * BLOCK as u64)
            .expect_err("past the end");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        // The short block, the direct read after it, the empty block and
        // the direct read after that.
        assert_eq!(opener.counts().preads, 4);
    }

    /// Four blocks are kept, for every file of the scan together, the one
    /// read longest ago going first; a read of a kept block keeps it.
    #[test]
    fn four_blocks_are_kept_least_recently_used_first_out() {
        let (_file, opener, h) = patterned(6 * BLOCK);
        let (_other_file, other_opener, other) = patterned(BLOCK);
        let mut blocks = Blocks::default();
        let mut buf = [0u8; 4];
        let mut read = |blocks: &mut Blocks, file: &Handle, id: usize, block: usize| {
            blocks
                .read_exact_at(file, id, &mut buf, (block * BLOCK + 1) as u64)
                .expect("reads");
            assert_eq!(buf.to_vec(), expected(block * BLOCK + 1, 4));
        };
        for block in 0..4 {
            read(&mut blocks, &h, 0, block);
        }
        assert_eq!(opener.counts().preads, 4);
        // Block 0 used again, then another file's block 0: block 1 goes.
        read(&mut blocks, &h, 0, 0);
        read(&mut blocks, &other, 1, 0);
        assert_eq!(
            (opener.counts().preads, other_opener.counts().preads),
            (4, 1)
        );
        for block in [0, 2, 3] {
            read(&mut blocks, &h, 0, block);
        }
        assert_eq!(opener.counts().preads, 4);
        read(&mut blocks, &h, 0, 1);
        assert_eq!(opener.counts().preads, 5);
        assert_eq!(blocks.kept.len(), KEPT);
        assert!(blocks.kept.iter().all(|(_, bytes)| bytes.len() == BLOCK));
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
