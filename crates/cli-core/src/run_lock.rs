//! The run lock (spec, "Run lock"; the maintainer, 2026-11-18): the
//! commands that change the host run one at a time, under an exclusive
//! `flock` on [`FILE`] in [`DIR`], which the kernel releases when the
//! holder ends, however it ends.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, FileExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

/// The lock's directory: in `/run`, a root-owned 0755 tmpfs, so only root
/// can make it (spec: why not `/var/lock`, which is world-writable).
pub const DIR: &str = "/run/sb";

/// The lock file in [`DIR`].
pub const FILE: &str = "run.lock";

/// The variable naming the lock's directory in sb's test builds (the
/// `testing` feature, never a release build), so tests never use the
/// host's.
#[cfg(feature = "testing")]
pub const DIR_VAR: &str = "SB_TEST_RUN_LOCK_DIR";

/// How often a waiting sb tries again (std's `File::try_lock`,
/// `flock(LOCK_EX | LOCK_NB)`,
/// <https://doc.rust-lang.org/1.100.0/std/fs/struct.File.html#method.try_lock>),
/// and checks for Ctrl-C or a signal. Why not the blocking `File::lock`:
/// a `flock` that waits can't be cancelled. signal-hook installs its
/// handlers with `SA_RESTART`, which restarts the call, and they run on
/// sb's signals thread, not the one waiting, so Ctrl-C couldn't end the
/// wait.
const POLL: Duration = Duration::from_millis(100);

/// The most of the holder's record a waiting sb reads.
const RECORD_LIMIT: usize = 256;

/// The lock's directory: [`DIR`], or a test build's [`DIR_VAR`] when set
/// and not empty.
#[must_use]
pub fn dir() -> PathBuf {
    #[cfg(feature = "testing")]
    if let Some(dir) = std::env::var_os(DIR_VAR).filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    PathBuf::from(DIR)
}

/// The sb holding the lock, as its record says. The record is sb's own:
/// no pidfile crate stores the command (fslock's `try_lock_with_pid`
/// writes the pid alone, besides a lock of its own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// `sb <command>`.
    pub command: String,
}

impl Holder {
    /// The record: the pid and the command, a line each.
    fn record(&self) -> String {
        format!("{}\n{}\n", self.pid, self.command)
    }

    /// The holder a record names, or none when it can't be read as one
    /// (being written, or not sb's).
    fn parse(record: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(record).ok()?;
        let mut lines = text.lines();
        let pid = lines.next()?.parse().ok()?;
        let command = lines.next().filter(|command| !command.is_empty())?;
        Some(Self {
            pid,
            command: command.to_owned(),
        })
    }
}

/// The line a waiting sb prints once on stderr (spec, "Waiting"), before
/// [`crate::exit::escaped`]: the holder's record is root's, but printed
/// text from a file is escaped all the same.
#[must_use]
pub fn waiting_line(holder: Option<&Holder>) -> String {
    match holder {
        Some(Holder { pid, command }) => format!(
            "another sb command is running ({command}, pid {pid}); waiting… (Ctrl-C to give up)"
        ),
        None => "another sb command is running; waiting… (Ctrl-C to give up)".to_owned(),
    }
}

/// How this run has the lock.
#[derive(Debug)]
pub enum Taken {
    /// Its own, until this is dropped (the file closed).
    Held {
        file: File,
        /// How long it waited.
        waited: Duration,
        /// The holder it waited for, when it waited and could read one.
        waited_for: Option<Holder>,
    },
    /// An ancestor's: this sb runs under its lock (spec, "Nesting").
    Nested(Holder),
}

/// Why the lock wasn't taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("run lock directory {}: {source}", path.display())]
    Dir { path: PathBuf, source: io::Error },
    #[error(
        "run lock directory {} must be a directory owned by the user running sb, not group- or world-writable",
        path.display()
    )]
    UnsafeDir { path: PathBuf },
    #[error("run lock {}: {source}", path.display())]
    File { path: PathBuf, source: io::Error },
    #[error("run lock {} must be a regular file owned by the user running sb", path.display())]
    UnsafeFile { path: PathBuf },
    /// Ctrl-C or a signal while waiting: the run ends by it.
    #[error("interrupted while waiting for the run lock")]
    Interrupted,
}

/// Take the lock in `dir` for `command` (`sb <command>`), waiting while
/// another sb holds it: `waiting` is called once, with the holder when its
/// record can be read, and every [`POLL`] the lock is tried again until
/// `token` is cancelled. An ancestor's lock is this run's
/// ([`Taken::Nested`]).
///
/// # Errors
///
/// [`LockError`]: the directory or file can't be made or opened, or isn't
/// safe; [`LockError::Interrupted`] once `token` is cancelled.
pub fn take(
    dir: &Path,
    command: &str,
    token: &CancellationToken,
    mut waiting: impl FnMut(Option<&Holder>),
) -> Result<Taken, LockError> {
    let file = open(dir)?;
    let started = Instant::now();
    let mut ancestors = None;
    let mut waited_for = None;
    let mut told = false;
    loop {
        match file.try_lock() {
            Ok(()) => {
                let own = Holder {
                    pid: std::process::id(),
                    command: command.to_owned(),
                };
                write_record(&file, &own).map_err(|source| LockError::File {
                    path: dir.join(FILE),
                    source,
                })?;
                return Ok(Taken::Held {
                    file,
                    waited: started.elapsed(),
                    waited_for,
                });
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let holder = read_record(&file);
                if let Some(holder) = &holder
                    && ancestors
                        .get_or_insert_with(|| ancestor_pids(std::os::unix::process::parent_id()))
                        .contains(&holder.pid)
                {
                    return Ok(Taken::Nested(holder.clone()));
                }
                if !told {
                    waiting(holder.as_ref());
                    told = true;
                    waited_for = holder;
                }
            }
            Err(std::fs::TryLockError::Error(source)) => {
                return Err(LockError::File {
                    path: dir.join(FILE),
                    source,
                });
            }
        }
        if token.is_cancelled() {
            return Err(LockError::Interrupted);
        }
        std::thread::sleep(POLL);
        if token.is_cancelled() {
            return Err(LockError::Interrupted);
        }
    }
}

/// The lock file, `dir` made first (0700) when missing; both checked:
/// `dir` a directory, not a link, owned by the user running sb and not
/// group- or world-writable; the file opened without following a link,
/// made 0600, a regular file of that user's. std opens it `O_CLOEXEC`, so
/// no child inherits it.
fn open(dir: &Path) -> Result<File, LockError> {
    let dir_error = |source| LockError::Dir {
        path: dir.to_owned(),
        source,
    };
    match std::fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // Not recursive: `/run` is there on a systemd host, and a
            // missing parent isn't sb's to make (tests make theirs).
            match DirBuilder::new().mode(0o700).create(dir) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(dir_error(err)),
            }
        }
        Err(err) => return Err(dir_error(err)),
    }
    let uid = crate::root::effective_uid();
    let meta = std::fs::symlink_metadata(dir).map_err(dir_error)?;
    if !meta.file_type().is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err(LockError::UnsafeDir {
            path: dir.to_owned(),
        });
    }
    let path = dir.join(FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(&path)
        .map_err(|source| LockError::File {
            path: path.clone(),
            source,
        })?;
    let meta = file.metadata().map_err(|source| LockError::File {
        path: path.clone(),
        source,
    })?;
    if !meta.file_type().is_file() || meta.uid() != uid {
        return Err(LockError::UnsafeFile { path });
    }
    Ok(file)
}

/// The file's contents replaced by `holder`'s record: written over the
/// old one first, then cut to its length, so a waiter reading between the
/// two sees the new record's lines first (the rest, the old one's tail, is
/// past the two lines [`Holder::parse`] reads), never a cut one.
fn write_record(file: &File, holder: &Holder) -> io::Result<()> {
    let record = holder.record();
    file.write_all_at(record.as_bytes(), 0)?;
    file.set_len(u64::try_from(record.len()).unwrap_or(u64::MAX))
}

/// The holder the file's record names, read up to [`RECORD_LIMIT`].
fn read_record(file: &File) -> Option<Holder> {
    let mut record = [0; RECORD_LIMIT];
    let read = file.read_at(&mut record, 0).ok()?;
    Holder::parse(&record[..read])
}

/// `pid` and its ancestors, nearest first, from `/proc` (procfs's
/// `Stat::ppid`, <https://docs.rs/procfs/0.18.0/procfs/process/struct.Stat.html#structfield.ppid>):
/// up to init, or as far as `/proc` can be read. At most 4096, so a loop
/// in what `/proc` gives can't hang sb.
fn ancestor_pids(pid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut next = pid;
    while next > 1 && found.len() < 4096 {
        found.push(next);
        let parent = i32::try_from(next)
            .ok()
            .and_then(|pid| procfs::process::Process::new(pid).ok())
            .and_then(|process| process.stat().ok())
            .and_then(|stat| u32::try_from(stat.ppid).ok());
        match parent {
            Some(parent) => next = parent,
            None => break,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn token() -> CancellationToken {
        CancellationToken::new()
    }

    /// Another open of the file, holding the lock (`flock` is per open
    /// file, so this process's own second open conflicts), with `record`.
    fn hold(dir: &Path, record: &str) -> File {
        let file = open(dir).expect("open");
        file.try_lock().expect("lock");
        file.set_len(0).expect("truncate");
        file.write_all_at(record.as_bytes(), 0).expect("record");
        file
    }

    #[test]
    fn a_free_lock_is_taken_at_once_and_records_its_holder() {
        let dir = tempfile::tempdir().expect("dir");
        let lock = dir.path().join("sb");
        let mut told = 0;
        let taken = take(&lock, "sb install", &token(), |_| told += 1).expect("taken");
        assert!(matches!(
            taken,
            Taken::Held {
                waited_for: None,
                ..
            }
        ));
        assert_eq!(told, 0);
        let record = std::fs::read_to_string(lock.join(FILE)).expect("record");
        assert_eq!(record, format!("{}\nsb install\n", std::process::id()));
        let mode =
            |path: &Path| std::fs::metadata(path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&lock), 0o700);
        assert_eq!(mode(&lock.join(FILE)), 0o600);
        // Dropped, it is free again.
        drop(taken);
        let again = take(&lock, "sb update", &token(), |_| told += 1).expect("again");
        assert!(matches!(again, Taken::Held { .. }));
        assert_eq!(told, 0);
    }

    #[test]
    fn a_held_lock_is_waited_for_and_named_once() {
        let dir = tempfile::tempdir().expect("dir");
        // Pid 1 is init: never this test's ancestor list (which stops
        // before it).
        let holder = hold(dir.path(), "1\nsb update\n");
        let cancel = token();
        let (told, waiting) = std::sync::mpsc::channel();
        let (result, held_for) = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                take(dir.path(), "sb install", &cancel, |holder| {
                    let _ = told.send(holder.cloned());
                })
            });
            // Released only once the waiter has said it waits, and two
            // polls later.
            let seen = waiting.recv().expect("told");
            let since = Instant::now();
            std::thread::sleep(POLL * 2);
            drop(holder);
            let held_for = since.elapsed();
            assert_eq!(waiting.try_recv().ok(), None, "told twice");
            (
                waiter.join().expect("waiter").map(|taken| (taken, seen)),
                held_for,
            )
        });
        let (taken, seen) = result.expect("taken");
        let expected = Holder {
            pid: 1,
            command: "sb update".to_owned(),
        };
        assert_eq!(seen, Some(expected.clone()));
        match taken {
            Taken::Held {
                waited, waited_for, ..
            } => {
                assert!(waited >= held_for, "{waited:?} {held_for:?}");
                assert_eq!(waited_for, Some(expected));
            }
            Taken::Nested(_) => panic!("nested"),
        }
    }

    #[test]
    fn a_cancelled_wait_is_interrupted() {
        let dir = tempfile::tempdir().expect("dir");
        let _holder = hold(dir.path(), "garbage");
        let token = token();
        let (told, waiting) = std::sync::mpsc::channel();
        let (result, seen) = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                take(dir.path(), "sb install", &token, |holder| {
                    let _ = told.send(holder.cloned());
                })
            });
            let seen = waiting.recv().expect("told");
            token.cancel();
            (waiter.join().expect("waiter"), seen)
        });
        assert!(matches!(result, Err(LockError::Interrupted)), "{result:?}");
        // An unreadable record: no holder named.
        assert_eq!(seen, None);
    }

    #[test]
    fn an_ancestors_lock_is_this_runs() {
        let dir = tempfile::tempdir().expect("dir");
        let parent = std::os::unix::process::parent_id();
        let _holder = hold(dir.path(), &format!("{parent}\nsb install\n"));
        let taken = take(dir.path(), "sb install", &token(), |_| panic!("waited")).expect("taken");
        assert!(
            matches!(&taken, Taken::Nested(holder) if holder.pid == parent),
            "{taken:?}"
        );
        // The holder's record is left as it was.
        let record = std::fs::read_to_string(dir.path().join(FILE)).expect("record");
        assert_eq!(record, format!("{parent}\nsb install\n"));
    }

    #[test]
    fn an_unsafe_directory_or_file_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let open_dir = dir.path().join("open");
        std::fs::create_dir(&open_dir).expect("dir");
        std::fs::set_permissions(&open_dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        assert!(matches!(
            take(&open_dir, "sb install", &token(), |_| {}),
            Err(LockError::UnsafeDir { .. })
        ));
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(dir.path(), &linked).expect("link");
        assert!(matches!(
            take(&linked, "sb install", &token(), |_| {}),
            Err(LockError::UnsafeDir { .. })
        ));
        // A link where the file goes isn't followed.
        let with_link = dir.path().join("with-link");
        DirBuilder::new()
            .mode(0o700)
            .create(&with_link)
            .expect("dir");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), with_link.join(FILE))
            .expect("link");
        assert!(matches!(
            take(&with_link, "sb install", &token(), |_| {}),
            Err(LockError::File { .. })
        ));
        assert!(!dir.path().join("elsewhere").exists());
        // Nor is a directory there.
        let with_dir = dir.path().join("with-dir");
        DirBuilder::new()
            .mode(0o700)
            .create(&with_dir)
            .expect("dir");
        std::fs::create_dir(with_dir.join(FILE)).expect("dir");
        assert!(take(&with_dir, "sb install", &token(), |_| {}).is_err());
    }

    /// Refused as well: a file where the directory goes, and, as root, a
    /// directory or file of another user's.
    #[test]
    fn a_wrong_kind_or_owner_is_refused() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("file");
        std::fs::write(&file, "").expect("file");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        assert!(matches!(
            take(&file, "sb install", &token(), |_| {}),
            Err(LockError::UnsafeDir { .. })
        ));
        if crate::root::effective_uid() != 0 {
            return;
        }
        let nobody = Some(65534);
        let theirs = dir.path().join("theirs");
        DirBuilder::new().mode(0o700).create(&theirs).expect("dir");
        std::os::unix::fs::chown(&theirs, nobody, nobody).expect("chown");
        assert!(matches!(
            take(&theirs, "sb install", &token(), |_| {}),
            Err(LockError::UnsafeDir { .. })
        ));
        let ours = dir.path().join("ours");
        DirBuilder::new().mode(0o700).create(&ours).expect("dir");
        std::fs::write(ours.join(FILE), "").expect("file");
        std::os::unix::fs::chown(ours.join(FILE), nobody, nobody).expect("chown");
        assert!(matches!(
            take(&ours, "sb install", &token(), |_| {}),
            Err(LockError::UnsafeFile { .. })
        ));
    }

    /// A shorter record over a longer one leaves exactly the new one, and
    /// the two lines it starts with parse as the new holder at any point.
    #[test]
    fn a_record_replaces_a_longer_one() {
        let dir = tempfile::tempdir().expect("dir");
        let file = open(dir.path()).expect("open");
        let long = Holder {
            pid: 4_000_000,
            command: "sb reinstall-python".to_owned(),
        };
        let short = Holder {
            pid: 12,
            command: "sb gha".to_owned(),
        };
        write_record(&file, &long).expect("long");
        // Between the write and the cut, as a waiter could read it.
        let mut between = short.record().into_bytes();
        between.extend_from_slice(&long.record().as_bytes()[between.len()..]);
        assert_eq!(Holder::parse(&between), Some(short.clone()));
        write_record(&file, &short).expect("short");
        let record = std::fs::read_to_string(dir.path().join(FILE)).expect("record");
        assert_eq!(record, short.record());
        assert_eq!(read_record(&file), Some(short));
    }

    #[test]
    fn a_record_parses_only_as_sb_writes_it() {
        let holder = Holder {
            pid: 1234,
            command: "sb install".to_owned(),
        };
        assert_eq!(Holder::parse(holder.record().as_bytes()), Some(holder));
        for record in [
            "",
            "1234",
            "1234\n",
            "x\nsb install\n",
            "1234\n\n",
            "-1\nsb\n",
        ] {
            assert_eq!(Holder::parse(record.as_bytes()), None, "{record:?}");
        }
        assert_eq!(Holder::parse(b"\xff\nsb\n"), None);
    }

    #[test]
    fn the_waiting_line_names_the_holder_when_known() {
        let holder = Holder {
            pid: 1234,
            command: "sb install".to_owned(),
        };
        assert_eq!(
            waiting_line(Some(&holder)),
            "another sb command is running (sb install, pid 1234); waiting… (Ctrl-C to give up)"
        );
        assert_eq!(
            waiting_line(None),
            "another sb command is running; waiting… (Ctrl-C to give up)"
        );
    }

    #[test]
    fn the_ancestors_are_this_process_and_its_parents() {
        let own = std::process::id();
        let found = ancestor_pids(own);
        assert_eq!(found.first(), Some(&own));
        assert!(found.contains(&std::os::unix::process::parent_id()));
        assert!(!found.contains(&1));
        assert_eq!(ancestor_pids(1), Vec::<u32>::new());
        assert_eq!(ancestor_pids(0), Vec::<u32>::new());
    }

    #[cfg(feature = "testing")]
    #[test]
    fn the_directory_is_run_sb_unless_a_test_build_moves_it() {
        // Read-only on the variable: a test that set it would race the
        // others in this binary.
        if std::env::var_os(DIR_VAR).is_none_or(|dir| dir.is_empty()) {
            assert_eq!(dir(), PathBuf::from(DIR));
        }
    }
}
