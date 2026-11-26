//! The run lock (spec, "Run lock"; the maintainer, 2026-11-18): the
//! commands that change the host run one at a time, under an exclusive
//! `flock` on [`FILE`] in [`DIR`], which the kernel releases when the
//! holder ends, however it ends.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, FileExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

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

/// Saltbox's backup lock, in its checkout (spec, "Run lock", Saltbox's
/// backup): Saltbox's `backup` and `backup2` roles touch
/// `<checkout>/backup.lock` once their sanity check has passed and remove
/// it when they end, however they end. It holds no pid and no `flock`, so
/// it says nothing of its own liveness: a backup killed outright leaves it.
const BACKUP_LOCK: &str = "backup.lock";

/// The playbook Saltbox's scheduled backup runs, in its checkout: cron's
/// `sudo ... ansible-playbook <checkout>/backup.yml --tags backup2` (or
/// `backup`; `roles/backup2/tasks/cron.yml`), as root.
const BACKUP_PLAYBOOK: &str = "backup.yml";

/// When the backup holding the lock in `checkout` started, as the lock's
/// mtime says, and its playbook's pid, while it runs: the lock is there
/// (looked at without following a link, as Ansible's `stat` does by
/// default) and `alive` gives the pid of a process running the backup's
/// playbook ([`BackupPlaybook`]). Liveness, not the lock's age, decides
/// (the maintainer, 2026-11-18): a lock left by a dead backup holds
/// nothing however young, and a backup past Saltbox's own age limits
/// still holds while it runs. sb never writes the file.
fn backup_running(
    checkout: &Path,
    alive: impl FnOnce() -> Option<i32>,
) -> Option<(SystemTime, i32)> {
    let since = std::fs::symlink_metadata(checkout.join(BACKUP_LOCK))
        .ok()?
        .modified()
        .ok()?;
    alive().map(|pid| (since, pid))
}

/// The process running Saltbox's backup playbook from a checkout, found
/// once and then held: procfs's `Process` keeps `/proc/<pid>` open, so a
/// later poll asks only it (`is_alive`, one `stat` read) and a pid reused
/// by another process can't pass for it; `/proc` is scanned again only once
/// it is gone (every process's `cmdline`, `status` and namespaces: procfs's
/// `all_processes`,
/// <https://docs.rs/procfs/0.18.0/procfs/process/fn.all_processes.html>).
struct BackupPlaybook<'a> {
    checkout: &'a Path,
    held: Option<procfs::process::Process>,
}

impl BackupPlaybook<'_> {
    /// The live backup playbook's pid, if one runs.
    fn alive(&mut self) -> Option<i32> {
        if let Some(process) = &self.held
            && process.is_alive()
        {
            return Some(process.pid());
        }
        self.held = find_backup_playbook(self.checkout);
        self.held.as_ref().map(procfs::process::Process::pid)
    }
}

/// A process on the host running Saltbox's backup playbook from
/// `checkout` ([`runs_backup_playbook`]), as root (its effective uid, from
/// `status`; the backup only runs so) and in sb's own mount namespace (not
/// a container's): any user or container can name its own process after
/// the playbook. A process that ends or can't be read meanwhile is
/// skipped; a `/proc` that can't be listed finds none, logged.
fn find_backup_playbook(checkout: &Path) -> Option<procfs::process::Process> {
    let processes = match procfs::process::all_processes() {
        Ok(processes) => processes,
        Err(err) => {
            tracing::warn!(error = %err, "can't list processes to look for a Saltbox backup");
            return None;
        }
    };
    let own_mount = mount_namespace(&procfs::process::Process::myself().ok()?)?;
    processes.flatten().find(|process| {
        process
            .cmdline()
            .is_ok_and(|args| runs_backup_playbook(&args, || process.cwd().ok(), checkout))
            && process.status().is_ok_and(|status| status.euid == 0)
            && mount_namespace(process) == Some(own_mount)
    })
}

/// `process`'s mount namespace's identifier (procfs's `namespaces`).
fn mount_namespace(process: &procfs::process::Process) -> Option<u64> {
    let namespaces = process.namespaces().ok()?;
    namespaces
        .0
        .get(std::ffi::OsStr::new("mnt"))
        .map(|namespace| namespace.identifier)
}

/// Whether `args` (a process's command line) run Saltbox's backup
/// playbook from `checkout`: the program is `ansible-playbook`, or a
/// Python running it as its script (the venv's entry point, the process
/// that runs the whole playbook); and one argument is
/// `<checkout>/backup.yml`, or a relative path to it from the process's
/// working directory (`cwd`, asked only then; `backup.yml`,
/// `./backup.yml`).
fn runs_backup_playbook(
    args: &[String],
    cwd: impl FnOnce() -> Option<PathBuf>,
    checkout: &Path,
) -> bool {
    let named = |arg: Option<&String>, name: &str| {
        arg.and_then(|arg| Path::new(arg).file_name())
            .and_then(|file| file.to_str())
            .is_some_and(|file| file == name || (name == "python" && file.starts_with("python")))
    };
    let ansible = named(args.first(), "ansible-playbook")
        || (named(args.first(), "python") && named(args.get(1), "ansible-playbook"));
    if !ansible {
        return false;
    }
    let whole = checkout.join(BACKUP_PLAYBOOK);
    if args.iter().any(|arg| Path::new(arg) == whole) {
        return true;
    }
    let relative: Vec<&Path> = args
        .iter()
        .map(Path::new)
        .filter(|arg| arg.is_relative() && arg.file_name() == Some(BACKUP_PLAYBOOK.as_ref()))
        .collect();
    !relative.is_empty()
        && cwd().is_some_and(|dir| relative.iter().any(|arg| dir.join(arg) == whole))
}

/// A wait for Saltbox's backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupWait {
    /// When the backup started: its lock's mtime.
    pub since: SystemTime,
    /// How long sb waited.
    pub waited: Duration,
    /// The pid of the backup's playbook when sb first saw it.
    pub pid: i32,
}

/// Wait while Saltbox's backup runs from `checkout`: its lock there and
/// its playbook alive ([`BackupPlaybook`]). `waiting` is called once, with
/// when it started; every [`POLL`] it is looked at again until the lock is
/// gone or no backup playbook runs, or `token` is cancelled. `None` when no
/// backup ran. Why polled, not an inotify watch or a pidfd: the token is
/// polled anyway, and neither wait is in the tree.
///
/// # Errors
///
/// [`LockError::Interrupted`] once `token` is cancelled while waiting.
pub fn wait_for_backup(
    checkout: &Path,
    token: &CancellationToken,
    waiting: impl FnMut(SystemTime),
) -> Result<Option<BackupWait>, LockError> {
    let mut playbook = BackupPlaybook {
        checkout,
        held: None,
    };
    wait_for_backup_with(checkout, token, waiting, || playbook.alive())
}

/// [`wait_for_backup`] with the liveness check `alive` (M-MOCKABLE-SYSCALLS:
/// the unit tests give theirs).
fn wait_for_backup_with(
    checkout: &Path,
    token: &CancellationToken,
    mut waiting: impl FnMut(SystemTime),
    mut alive: impl FnMut() -> Option<i32>,
) -> Result<Option<BackupWait>, LockError> {
    let started = Instant::now();
    let mut seen = None;
    while let Some((since, pid)) = backup_running(checkout, &mut alive) {
        if seen.is_none() {
            waiting(since);
            seen = Some((since, pid));
        }
        if token.is_cancelled() {
            return Err(LockError::Interrupted);
        }
        std::thread::sleep(POLL);
        if token.is_cancelled() {
            return Err(LockError::Interrupted);
        }
    }
    Ok(seen.map(|(since, pid)| BackupWait {
        since,
        waited: started.elapsed(),
        pid,
    }))
}

/// The line a sb waiting for Saltbox's backup prints once on stderr
/// (spec, "Run lock"), `since` the backup's start as sb shows a time.
#[must_use]
pub fn backup_waiting_line(since: &str) -> String {
    format!("a Saltbox backup is running (since {since}); waiting… (Ctrl-C to give up)")
}

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
    /// Ctrl-C or a signal while waiting, for the run lock or Saltbox's
    /// backup: the run ends by it.
    #[error("interrupted while waiting")]
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
/// a child sb starts holds it only until its exec: a child forked while
/// the lock is held shares the lock's open file until then, and the lock
/// stays taken that long after sb lets it go (the inheritance
/// [`crate::fork_lock`] describes). sb holds it for the whole run, so
/// that moment's delay to a waiting sb is all it costs.
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

    /// Under the fork lock's exclusive side, from the first open to the
    /// second take: a child another test forked meanwhile would share the
    /// lock's open file until its exec ([`open`]), and still hold the lock
    /// when it is taken again. Every child this crate's unit tests start
    /// takes the shared side, as sb-exec's children do; the fork lock's own
    /// tests wait their turn (`one_at_a_time`).
    #[test]
    fn a_free_lock_is_taken_at_once_and_records_its_holder() {
        let dir = tempfile::tempdir().expect("dir");
        let lock = dir.path().join("sb");
        let mut told = 0;
        let _one = crate::fork_lock::one_at_a_time();
        crate::fork_lock::writing(|| {
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
        });
    }

    /// std opens the lock close-on-exec: no program sb starts keeps it.
    #[test]
    fn the_lock_file_is_closed_on_exec() {
        let dir = tempfile::tempdir().expect("dir");
        let file = open(dir.path()).expect("open");
        let flags = rustix::io::fcntl_getfd(&file).expect("flags");
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC), "{flags:?}");
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
            // Read before the release: once it's released the waiter can
            // take the lock and stop its clock before this thread reads.
            let held_for = since.elapsed();
            drop(holder);
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

    /// The directory is made, but not its parent (`/run` isn't sb's to
    /// make): a missing parent is the directory's error, naming it, and
    /// nothing is created.
    #[test]
    fn a_missing_parent_is_not_made() {
        let dir = tempfile::tempdir().expect("dir");
        let lock = dir.path().join("run/sb");
        let err = open(&lock).expect_err("refused");
        assert!(
            matches!(&err, LockError::Dir { path, source }
                if *path == lock && source.kind() == io::ErrorKind::NotFound),
            "{err:?}"
        );
        assert!(
            err.to_string()
                .starts_with(&format!("run lock directory {}: ", lock.display())),
            "{err}"
        );
        assert!(!dir.path().join("run").exists());
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
        if !crate::testing::root_tests() {
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

    /// A stand-in backup lock in `dir`, its mtime `at`.
    fn backup_file(dir: &Path, at: SystemTime) -> PathBuf {
        let path = dir.join(BACKUP_LOCK);
        File::create(&path)
            .expect("file")
            .set_modified(at)
            .expect("mtime");
        path
    }

    /// The lock and a live backup playbook hold it, however old the lock
    /// is; the lock alone, however young, or a playbook alone, hold
    /// nothing; a link there is the link's own, not followed.
    #[test]
    fn a_backup_holds_its_lock_while_its_playbook_runs() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(backup_running(dir.path(), || Some(7)), None);
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        backup_file(dir.path(), old);
        assert_eq!(backup_running(dir.path(), || Some(7)), Some((old, 7)));
        assert_eq!(backup_running(dir.path(), || None), None);
        backup_file(dir.path(), SystemTime::now());
        assert_eq!(backup_running(dir.path(), || None), None);
        let path = dir.path().join(BACKUP_LOCK);
        std::fs::remove_file(&path).expect("rm");
        std::os::unix::fs::symlink("/nonexistent-target", &path).expect("link");
        assert!(backup_running(dir.path(), || Some(7)).is_some());
    }

    /// Saltbox's cron line, as the venv's Python runs it and as the
    /// program itself, and the forms that aren't its backup from this
    /// checkout.
    #[test]
    fn the_backup_playbook_is_known_by_its_arguments() {
        let checkout = Path::new("/srv/git/saltbox");
        let args = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        let no_cwd = || -> Option<PathBuf> { panic!("cwd asked") };
        for line in [
            &[
                "/srv/ansible/venv/bin/python3",
                "/srv/ansible/venv/bin/ansible-playbook",
                "/srv/git/saltbox/backup.yml",
                "--tags",
                "backup2",
            ][..],
            &[
                "python3.12",
                "ansible-playbook",
                "/srv/git/saltbox/backup.yml",
            ],
            &["ansible-playbook", "/srv/git/saltbox/backup.yml"],
            &[
                "/usr/local/bin/ansible-playbook",
                "/srv/git/saltbox/./backup.yml",
            ],
        ] {
            assert!(
                runs_backup_playbook(&args(line), no_cwd, checkout),
                "{line:?}"
            );
        }
        // Relative, run from the checkout: by the working directory.
        for relative in ["backup.yml", "./backup.yml"] {
            let line = args(&["ansible-playbook", relative, "--tags", "backup"]);
            assert!(runs_backup_playbook(
                &line,
                || Some(checkout.to_owned()),
                checkout
            ));
            assert!(!runs_backup_playbook(
                &line,
                || Some(PathBuf::from("/tmp")),
                checkout
            ));
            assert!(!runs_backup_playbook(&line, || None, checkout));
        }
        for line in [
            &[
                "ansible-playbook",
                "/srv/git/saltbox/saltbox.yml",
                "--tags",
                "backup",
            ][..],
            &["ansible-playbook", "/elsewhere/backup.yml"],
            // The program isn't ansible-playbook, whatever its arguments.
            &["grep", "ansible-playbook", "/srv/git/saltbox/backup.yml"],
            &[
                "sudo",
                "/usr/local/bin/ansible-playbook",
                "/srv/git/saltbox/backup.yml",
            ],
            &[
                "less",
                "--x=/a/ansible-playbook",
                "/srv/git/saltbox/backup.yml",
            ],
            &["not-ansible-playbook", "/srv/git/saltbox/backup.yml"],
            &[
                "python3",
                "-c",
                "ansible-playbook",
                "/srv/git/saltbox/backup.yml",
            ],
            &[],
        ] {
            assert!(
                !runs_backup_playbook(&args(line), || None, checkout),
                "{line:?}"
            );
        }
    }

    /// A root process of this test's own, named and given arguments as the
    /// backup's, is found and held until it ends; none is for another
    /// checkout. Not root, the test's process isn't one sb would count.
    #[test]
    fn a_live_backup_playbook_is_found_and_held() {
        use std::os::unix::process::CommandExt as _;
        let dir = tempfile::tempdir().expect("dir");
        let playbook = dir.path().join(BACKUP_PLAYBOOK);
        // Blocked in `read` on a pipe held here: no child of its own.
        // Started under the fork lock, as every child of these tests is.
        let mut child = crate::fork_lock::forking(|| {
            std::process::Command::new("sh")
                .arg0("ansible-playbook")
                .args(["-c", "read -r line", "sh"])
                .arg(&playbook)
                .stdin(std::process::Stdio::piped())
                .spawn()
        })
        .expect("stand-in");
        let pid = i32::try_from(child.id()).expect("pid");
        let root = crate::root::effective_uid() == 0;
        let mut held = BackupPlaybook {
            checkout: dir.path(),
            held: None,
        };
        let found = (0..50).find_map(|_| {
            let seen = held.alive();
            if seen.is_none() {
                std::thread::sleep(Duration::from_millis(20));
            }
            seen
        });
        let elsewhere = tempfile::tempdir().expect("dir");
        let other = find_backup_playbook(elsewhere.path()).map(|process| process.pid());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(other, None);
        // Only a root process is looked for: a run that isn't root checks
        // that it finds none, and ends there; it skips nothing, so it
        // isn't listed. The euid, not root_tests(): the stand-in is found
        // whenever this process is root, asked for or not.
        if !root {
            assert_eq!(found, None);
            return;
        }
        assert_eq!(found, Some(pid));
        assert!(held.held.is_some());
        // Ended: the held process says so, and no other is found.
        assert_eq!(held.alive(), None);
    }

    #[test]
    fn a_running_backup_is_waited_for_once_and_named() {
        let dir = tempfile::tempdir().expect("dir");
        let since = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let path = backup_file(dir.path(), since);
        let cancel = token();
        let (told, waiting) = std::sync::mpsc::channel();
        let (result, seen, held_for) = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                wait_for_backup_with(
                    dir.path(),
                    &cancel,
                    |at| {
                        let _ = told.send(at);
                    },
                    || Some(7),
                )
            });
            let seen = waiting.recv().expect("told");
            let from = Instant::now();
            std::thread::sleep(POLL * 2);
            // Read before the removal, as above.
            let held_for = from.elapsed();
            std::fs::remove_file(&path).expect("done");
            assert_eq!(waiting.try_recv().ok(), None, "told twice");
            (waiter.join().expect("waiter"), seen, held_for)
        });
        assert_eq!(seen, since);
        let waited = result.expect("waited").expect("a wait");
        assert_eq!(waited.since, since);
        assert_eq!(waited.pid, 7);
        assert!(waited.waited >= held_for, "{waited:?} {held_for:?}");
        // A lock with no live playbook: no wait, nothing told.
        backup_file(dir.path(), SystemTime::now());
        let none =
            wait_for_backup_with(dir.path(), &cancel, |_| panic!("told"), || None).expect("ok");
        assert_eq!(none, None);
    }

    #[test]
    fn a_cancelled_backup_wait_is_interrupted() {
        let dir = tempfile::tempdir().expect("dir");
        let path = backup_file(dir.path(), SystemTime::now());
        let cancel = token();
        let (told, waiting) = std::sync::mpsc::channel();
        let result = std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                wait_for_backup_with(
                    dir.path(),
                    &cancel,
                    |_| {
                        let _ = told.send(());
                    },
                    || Some(7),
                )
            });
            waiting.recv().expect("told");
            cancel.cancel();
            waiter.join().expect("waiter")
        });
        assert!(matches!(result, Err(LockError::Interrupted)), "{result:?}");
        assert!(path.exists(), "sb never touches Saltbox's file");
    }

    #[test]
    fn the_backup_line_names_its_start() {
        assert_eq!(
            backup_waiting_line("2026-11-18 03:00:12 CET"),
            "a Saltbox backup is running (since 2026-11-18 03:00:12 CET); waiting… (Ctrl-C to give up)"
        );
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
