//! The fact editor's session (tui spec, "Editors", Session; sb-go
//! facts/session.go, with decision 38's lock timing): the facts
//! directory pinned as an fd, `.fact-editor.lock`, the catalog read at
//! open, and Saltbox's `<role>.ini.lock`, taken only while a role is
//! saved ([`super::apply`]).
//!
//! Every file operation is relative to the pinned directory fd and
//! refuses symlinks (`openat` with `O_NOFOLLOW`, `statat` with
//! `AT_SYMLINK_NOFOLLOW`), through rustix
//! (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.openat.html>,
//! <https://docs.rs/rustix/1.1.6/rustix/fs/fn.statat.html>,
//! <https://docs.rs/rustix/1.1.6/rustix/fs/fn.flock.html>). The locks
//! are Saltbox's: `saltbox_facts.py` takes `<role>.ini.lock` with
//! `fcntl.flock(LOCK_EX|LOCK_NB)`, polled every 0.05 s for up to 30 s
//! (<https://github.com/saltyorg/Saltbox/blob/master/library/saltbox_facts.py>).
//!
//! Why not std's `File::try_lock`: it "currently corresponds to" `flock`
//! on Unix and may change
//! (<https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock>);
//! excluding Saltbox's module needs `flock(2)` exactly. Why not
//! `openat2` with `RESOLVE_NO_SYMLINKS` for the directory
//! (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.openat2.html>): it needs
//! Linux 5.6 or later and can be blocked by seccomp, where the
//! component walk is sb-go's and works everywhere. Why not cap-std: its
//! `FollowSymlinks::No`
//! (<https://docs.rs/cap-fs-ext/latest/cap_fs_ext/trait.OpenOptionsFollowExt.html>)
//! covers only the last component, so the directory still needs the
//! walk, and it is a new dependency family for what rustix does already
//! (tui spec, Tech Stack).

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags, Stat};
use rustix::io::Errno;
use sb_cli_core::config::{MAX_FILE_BYTES, escape_controls};

use super::Role;
use super::document::{Document, DocumentError};

/// The editors' lock, sb-go's name, so sb and sb-go exclude each other.
pub const EDITOR_LOCK: &str = ".fact-editor.lock";

/// How long a save waits for Saltbox's role lock (`saltbox_facts.py`'s
/// `LOCK_TIMEOUT_SECONDS`), and how often it tries
/// (`LOCK_POLL_INTERVAL_SECONDS`; the module doc's link).
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_POLL: Duration = Duration::from_millis(50);

/// A lock file: opened to read and write, created when missing.
const LOCK_FLAGS: OFlags = OFlags::CREATE.union(OFlags::RDWR);

/// Each directory of the path: never through a symlink.
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// Every file opened in the directory: never through a symlink, never
/// blocking on a FIFO, never inherited by a child.
const FILE_FLAGS: OFlags = OFlags::NOFOLLOW
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

/// Why the session couldn't open. Names are escaped (cli-core's rule).
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("facts directory must be an existing directory without symlinks: {0}")]
    Directory(#[source] io::Error),
    #[error("another fact editor is active")]
    EditorActive,
    #[error("acquire editor lock: {0}")]
    EditorLock(#[source] FileError),
    #[error("catalog facts directory: {0}")]
    List(#[source] io::Error),
    #[error("catalog {}: {cause}", escape_controls(.role))]
    Role {
        role: String,
        #[source]
        cause: FileError,
    },
}

/// Why one file of the facts directory couldn't be used.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("facts file {} must not be a symlink", escape_controls(.0))]
    Symlink(String),
    #[error("facts file {} must be a regular file", escape_controls(.0))]
    NotRegular(String),
    #[error("facts file {} changed while it was opened or read", escape_controls(.0))]
    Changed(String),
    #[error("facts file {} is bigger than {MAX_FILE_BYTES} bytes", escape_controls(.0))]
    TooBig(String),
    #[error("facts file name {} is not UTF-8", escape_controls(.0))]
    Name(String),
    #[error("facts file .ini names no app")]
    EmptyName,
    #[error("open facts file {}: {source}", escape_controls(.name))]
    Io {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Document(#[from] DocumentError),
}

impl FileError {
    fn io(name: &str, source: impl Into<io::Error>) -> Self {
        Self::Io {
            name: name.to_owned(),
            source: source.into(),
        }
    }

    /// Whether the file isn't there (a role Saltbox deleted).
    pub(super) fn is_not_found(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
    }
}

/// A file's identity: its device and inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Id {
    dev: u64,
    ino: u64,
}

impl From<&Stat> for Id {
    fn from(stat: &Stat) -> Self {
        Self {
            dev: stat.st_dev,
            ino: stat.st_ino,
        }
    }
}

/// One role's file as the session last read it: the baseline a save
/// checks against. Its `Debug` leaves the bytes out (they hold the
/// values).
#[derive(Clone)]
pub(super) struct RoleState {
    pub(super) data: Vec<u8>,
    /// The file's identity as read. After a save it is the old file's; a
    /// save reads the file again under the lock before it relies on it.
    pub(super) id: Id,
    pub(super) uid: u32,
    pub(super) gid: u32,
    /// The permission bits, setuid, setgid and sticky included.
    pub(super) mode: u32,
    pub(super) role: Role,
}

impl std::fmt::Debug for RoleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleState")
            .field("id", &self.id)
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

/// A held `<role>.ini.lock`; dropping it releases the lock.
#[derive(Debug)]
pub(super) struct RoleLock {
    /// Held while the role is saved; closing it releases the lock.
    pub(super) _fd: OwnedFd,
    pub(super) id: Id,
}

/// What a save does while Saltbox holds a role's lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Keep waiting, up to [`LOCK_TIMEOUT`].
    Keep,
    /// Stop the save: this role and the ones after it are left unattempted.
    Cancel,
}

/// The answer to the "Before / On disk" dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftChoice {
    /// Adopt the disk version and drop the role's staged changes.
    Reload,
    /// Keep the changes staged and write nothing for the role.
    Cancel,
}

/// A role that changed on disk since the session read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drift {
    /// The role as the session read it.
    pub before: Role,
    /// The role on disk now; `None` when the file is gone.
    pub after: Option<Role>,
}

/// The fact editor's answers during a save. The session calls them
/// with no role lock held, except [`SaveHooks::waiting`], which runs while
/// Saltbox holds the lock (sb holds none of its own then).
///
/// The save borrows the session, so a screen takes [`Session::catalog`]
/// before it to draw the page under its dialogs, and draws and reads
/// input inside the hooks: `waiting` returns at once (the save sleeps
/// 50 ms between tries), `drift` returns once the user has answered.
pub trait SaveHooks {
    /// Called on each try while Saltbox holds `role`'s lock (the "waiting
    /// for Saltbox" dialog), with the time waited so far. A cancel from
    /// the dialog, Ctrl+C included, is [`Wait::Cancel`].
    fn waiting(&mut self, role: &str, waited: Duration) -> Wait;
    /// Called when `role` changed on disk; the lock is released by then.
    fn drift(&mut self, role: &str, drift: &Drift) -> DriftChoice;
    /// Whether to stop (Ctrl+C or SIGTERM): asked before each role and on
    /// each try at a held lock; the role being committed finishes first.
    fn stopped(&mut self) -> bool;
}

/// An open fact editor session: the pinned facts directory, the editor
/// lock (held until drop) and the roles as read.
#[derive(Debug)]
pub struct Session {
    pub(super) dir: OwnedFd,
    /// Held for the session's life; dropping it releases the lock.
    _editor: OwnedFd,
    pub(super) roles: BTreeMap<String, RoleState>,
    pub(super) lock_timeout: Duration,
    pub(super) seam: super::apply::Seam,
}

impl Session {
    /// Opens the facts directory `path` (absolute): every component
    /// without a symlink, the editor lock taken without waiting, and every
    /// `<role>.ini` read. No role lock is taken.
    ///
    /// # Errors
    ///
    /// [`SessionError`]: a missing or symlinked path, another editor, and
    /// any role file that can't be read or parsed, naming it.
    pub fn open(path: &Path) -> Result<Self, SessionError> {
        let dir = open_directory(path).map_err(SessionError::Directory)?;
        let editor = open_regular(&dir, EDITOR_LOCK, LOCK_FLAGS, Mode::from_raw_mode(0o640))
            .map_err(SessionError::EditorLock)?;
        match rustix::fs::flock(&editor, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => return Err(SessionError::EditorActive),
            Err(err) => return Err(SessionError::EditorLock(FileError::io(EDITOR_LOCK, err))),
        }
        let mut roles = BTreeMap::new();
        for role in role_names(&dir)? {
            let state = read_role(&dir, &role).map_err(|cause| SessionError::Role {
                role: role.clone(),
                cause,
            })?;
            roles.insert(role, state);
        }
        tracing::info!(roles = roles.len(), "fact editor session opened");
        Ok(Self {
            dir,
            _editor: editor,
            roles,
            lock_timeout: LOCK_TIMEOUT,
            seam: super::apply::Seam::default(),
        })
    }

    /// The roles, sorted by name, each as [`Document::role`] lists it.
    #[must_use]
    pub fn catalog(&self) -> Vec<Role> {
        self.roles
            .values()
            .map(|state| state.role.clone())
            .collect()
    }

    /// Takes `role`'s Saltbox lock, trying every 50 ms for up to the lock
    /// timeout. `None`: the hooks cancelled the wait.
    pub(super) fn lock_role(
        &self,
        role: &str,
        hooks: &mut dyn SaveHooks,
    ) -> Result<Option<RoleLock>, super::apply::SaveError> {
        let name = format!("{role}.ini.lock");
        let fd = open_regular(&self.dir, &name, LOCK_FLAGS, Mode::from_raw_mode(0o640))?;
        let start = Instant::now();
        let mut logged = false;
        loop {
            match rustix::fs::flock(&fd, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(Errno::WOULDBLOCK) => {}
                Err(err) => return Err(FileError::io(&name, err).into()),
            }
            let waited = start.elapsed();
            if waited >= self.lock_timeout {
                tracing::warn!(role, "timed out waiting for Saltbox's role lock");
                return Err(super::apply::SaveError::LockTimeout);
            }
            if !logged {
                tracing::info!(role, "waiting for Saltbox's role lock");
                logged = true;
            }
            if hooks.stopped() || hooks.waiting(role, waited) == Wait::Cancel {
                tracing::info!(role, "wait for Saltbox's role lock cancelled");
                return Ok(None);
            }
            std::thread::sleep(LOCK_POLL);
        }
        // The name must still be the file locked: Saltbox's module locks
        // whatever the name is when it opens it.
        let id = Id::from(&rustix::fs::fstat(&fd).map_err(|err| FileError::io(&name, err))?);
        same_file(&self.dir, &name, id)?;
        tracing::info!(
            role,
            waited_ms = start.elapsed().as_millis(),
            "role lock taken"
        );
        Ok(Some(RoleLock { _fd: fd, id }))
    }
}

/// The roles in the pinned directory `dir`: each `<role>.ini` name
/// without its suffix, sorted. One `readdir`; no file is opened.
///
/// # Errors
///
/// [`SessionError::List`] when the directory can't be read, and
/// [`SessionError::Role`] for a name that isn't UTF-8 or is `.ini` alone.
pub(super) fn role_names(dir: &OwnedFd) -> Result<Vec<String>, SessionError> {
    let mut names = Vec::new();
    let mut entries =
        rustix::fs::Dir::read_from(dir).map_err(|err| SessionError::List(err.into()))?;
    while let Some(entry) = entries.read() {
        let entry = entry.map_err(|err| SessionError::List(err.into()))?;
        let bytes = entry.file_name().to_bytes();
        let Some(role) = bytes.strip_suffix(b".ini") else {
            continue;
        };
        let role = std::str::from_utf8(role).map_err(|_| {
            let name = String::from_utf8_lossy(bytes).into_owned();
            SessionError::Role {
                role: name.clone(),
                cause: FileError::Name(name),
            }
        })?;
        if role.is_empty() {
            return Err(SessionError::Role {
                role: ".ini".to_owned(),
                cause: FileError::EmptyName,
            });
        }
        names.push(role.to_owned());
    }
    names.sort();
    Ok(names)
}

/// `path` opened from `/` one component at a time, each with
/// `O_NOFOLLOW|O_DIRECTORY`, so a symlink anywhere is refused.
pub(super) fn open_directory(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path is not absolute",
        ));
    }
    let mut fd = rustix::fs::open("/", DIRECTORY_FLAGS, Mode::empty())?;
    for component in path.components() {
        if let Component::Normal(_) | Component::ParentDir = component {
            fd = rustix::fs::openat(&fd, component.as_os_str(), DIRECTORY_FLAGS, Mode::empty())?;
        }
    }
    Ok(fd)
}

/// `name` in `dir`, opened with `flags` (and `mode` when created): never
/// through a symlink, never anything but a regular file, and the same file
/// before, at and after the open. `O_NONBLOCK`, so a FIFO put in its place
/// can't hang the open.
pub(super) fn open_regular(
    dir: &OwnedFd,
    name: &str,
    flags: OFlags,
    mode: Mode,
) -> Result<OwnedFd, FileError> {
    let before = match rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => {
            check_type(name, &stat)?;
            Some(Id::from(&stat))
        }
        // Missing: created now, or the open below fails as missing.
        Err(Errno::NOENT) => None,
        Err(err) => return Err(FileError::io(name, err)),
    };
    let fd = rustix::fs::openat(dir, name, flags.union(FILE_FLAGS), mode)
        .map_err(|err| FileError::io(name, err))?;
    let opened = rustix::fs::fstat(&fd).map_err(|err| FileError::io(name, err))?;
    check_type(name, &opened)?;
    let id = Id::from(&opened);
    if before.is_some_and(|before| before != id) {
        return Err(FileError::Changed(name.to_owned()));
    }
    same_file(dir, name, id)?;
    Ok(fd)
}

fn check_type(name: &str, stat: &Stat) -> Result<(), FileError> {
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::RegularFile => Ok(()),
        FileType::Symlink => Err(FileError::Symlink(name.to_owned())),
        _ => Err(FileError::NotRegular(name.to_owned())),
    }
}

/// Whether `name` in `dir` is still the regular file `id`.
pub(super) fn same_file(dir: &OwnedFd, name: &str, id: Id) -> Result<(), FileError> {
    let stat = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|err| FileError::io(name, err))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || Id::from(&stat) != id {
        return Err(FileError::Changed(name.to_owned()));
    }
    Ok(())
}

/// `<role>.ini` read and parsed: at most cli-core's 1 MiB, the same file
/// throughout.
pub(super) fn read_role(dir: &OwnedFd, role: &str) -> Result<RoleState, FileError> {
    let name = format!("{role}.ini");
    let fd = open_regular(dir, &name, OFlags::RDONLY, Mode::empty())?;
    let stat = rustix::fs::fstat(&fd).map_err(|err| FileError::io(&name, err))?;
    let mut data = Vec::new();
    std::fs::File::from(fd)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|err| FileError::io(&name, err))?;
    if data.len() > MAX_FILE_BYTES {
        return Err(FileError::TooBig(name));
    }
    let id = Id::from(&stat);
    same_file(dir, &name, id)?;
    let role = Document::parse(&data)?.role(role);
    Ok(RoleState {
        data,
        id,
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: stat.st_mode & 0o7777,
        role,
    })
}

#[cfg(test)]
mod tests;
