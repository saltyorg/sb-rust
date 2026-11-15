//! Saltbox's apt repositories (host spec, "apt", `add_repositories`; sb-go's
//! `addAptRepositoriesForRelease`, host/apt.go:297-425), with the ops
//! spec's fenced cleanup (decisions 5 and 28): the `sources.list.d`
//! entries are moved aside into a root-only
//! `/var/backups/sb/apt-sources/<run id>/` through directory handles, never
//! deleted, and the files sb writes are saved atomically.
//!
//! Every file sb reads is read, as bytes (sb-go reads bytes), before
//! anything moves, so an unreadable file fails the step while the
//! third-party sources are still in place.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::io::{self, Read as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

/// The releases sb configures, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Release {
    /// jammy: the legacy `sources.list`.
    Legacy,
    /// noble and resolute: DEB822, keeping `ubuntu.sources`.
    Deb822,
}

/// Why the repositories couldn't be set up, in sb-go's words where sb-go
/// has them.
#[derive(Debug, thiserror::Error)]
pub enum SourcesError {
    /// `lsb_release -sc` failed.
    #[error("error getting Ubuntu release codename: {0}")]
    Codename(String),
    /// A codename sb doesn't configure; nothing has changed.
    #[error("unsupported Ubuntu release: {0}")]
    Unsupported(String),
    /// A directory that isn't what it claims: nothing has moved.
    #[error("refusing to clean {}: {reason}", dir.display())]
    Refused { dir: PathBuf, reason: String },
    /// An entry that can't be moved aside (a subdirectory, as sb-go's
    /// removal fails on one); nothing has moved.
    #[error("error removing {}: {cause}", path.display())]
    Remove { path: PathBuf, cause: io::Error },
    /// A file couldn't be read.
    #[error("error reading {}: {cause}", path.display())]
    Read { path: PathBuf, cause: io::Error },
    /// A file couldn't be written.
    #[error("error writing to {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
    /// An entry couldn't be moved into the backup.
    #[error("error moving {} to {}: {cause}", path.display(), backup.display())]
    Move {
        path: PathBuf,
        backup: PathBuf,
        cause: io::Error,
    },
    /// `ubuntu.sources` couldn't be read.
    #[error("error checking ubuntu.sources mirror configuration: {0}")]
    Mirror(Box<SourcesError>),
    /// `ubuntu-archive.sources` couldn't be written.
    #[error("error writing ubuntu-archive.sources: {0}")]
    Archive(Box<SourcesError>),
}

/// Moves an entry between two directory handles (`renameat`); a field so
/// tests can make it fail with `EXDEV` (M-MOCKABLE-SYSCALLS).
pub(crate) type Rename = fn(BorrowedFd<'_>, &CStr, BorrowedFd<'_>, &CStr) -> rustix::io::Result<()>;

/// Where the files are: `/etc/apt` and `/var/backups`, and the owner the
/// directories must have (root). Other values exist for tests only.
#[derive(Debug, Clone)]
pub(crate) struct Roots {
    pub(crate) etc_apt: PathBuf,
    pub(crate) var_backups: PathBuf,
    pub(crate) owner: u32,
    pub(crate) rename: Rename,
}

impl Default for Roots {
    fn default() -> Self {
        Self {
            etc_apt: PathBuf::from("/etc/apt"),
            var_backups: PathBuf::from("/var/backups"),
            owner: 0,
            rename,
        }
    }
}

/// What a run did: the entries it moved and where. The caller owns it, so
/// it still holds what moved when a later step fails.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) moved: Vec<String>,
    pub(crate) backup: Option<PathBuf>,
}

/// The directory cleared, under `/etc/apt`.
const SOURCES_D: &str = "sources.list.d";
/// The installer's DEB822 file, kept on noble and resolute.
const KEPT: &str = "ubuntu.sources";
/// The file sb adds beside a custom mirror.
const ARCHIVE: &str = "ubuntu-archive.sources";
/// The most of a link's target `remove_mentioning` reads: the target may
/// live outside the root-only `sources.list.d`, and an apt list is a few
/// lines (host spec, H3, amended 2026-11-15).
const MAX_LINKED: usize = 1 << 20;
/// sb's backup folder under `/var/backups`.
const BACKUP_ROOT: &str = "sb";
/// `add_repositories`' folder under it.
const APT_SOURCES: &str = "apt-sources";
/// A temporary file's suffix: apt ignores a name ending in `~` silently
/// (`Dir::Ignore-Files-Silently` "~$", apt 2.4, 2.8 and 3.2;
/// docs/platform-notes/apt.md).
const TEMP_SUFFIX: &str = "~";

/// sb-go's release rule.
pub(crate) fn release(codename: &str) -> Result<Release, SourcesError> {
    match codename {
        "jammy" => Ok(Release::Legacy),
        "noble" | "resolute" => Ok(Release::Deb822),
        other => Err(SourcesError::Unsupported(other.to_string())),
    }
}

/// The cleanup and the release's files, under `roots`, with what moved in
/// `report`: nothing changes for an unsupported codename, a bad run id, a
/// refused directory, a file that can't be read or an entry that can't be
/// moved.
pub(crate) fn apply(
    roots: &Roots,
    codename: &str,
    run_id: &str,
    report: &mut Report,
) -> Result<(), SourcesError> {
    let release = release(codename)?;
    check_names(roots, APT_SOURCES, run_id)?;
    let sources_d = roots.etc_apt.join(SOURCES_D);
    let dir = match open_dir(&sources_d, roots.owner) {
        Ok(dir) => Some(dir),
        Err(Opened::Missing) => None,
        Err(failed) => return Err(failed.error(&sources_d)),
    };
    match release {
        Release::Legacy => {
            let existing = read_sources_list(&roots.etc_apt.join("sources.list"))?;
            if let Some(dir) = &dir {
                clean(roots, dir, &sources_d, release, run_id, report)?;
            }
            save(
                &existing.dir,
                &existing.name,
                &existing.path,
                &legacy_lines(&existing.bytes, codename),
                existing.owner,
            )
        }
        Release::Deb822 => {
            let kept = sources_d.join(KEPT);
            let official = match &dir {
                Some(dir) => {
                    uses_archive(dir, &kept).map_err(|err| SourcesError::Mirror(Box::new(err)))?
                }
                None => false,
            };
            let Some(dir) = dir else {
                return Err(SourcesError::Archive(Box::new(SourcesError::Write {
                    path: sources_d.join(ARCHIVE),
                    cause: io::ErrorKind::NotFound.into(),
                })));
            };
            clean(roots, &dir, &sources_d, release, run_id, report)?;
            if official {
                return Ok(());
            }
            // Written through the checked handle's /proc/self/fd link, so a
            // swapped path can't redirect it (proc_pid_fd(5):
            // <https://man7.org/linux/man-pages/man5/proc_pid_fd.5.html>).
            let through = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
            save(
                &through,
                OsStr::new(ARCHIVE),
                &sources_d.join(ARCHIVE),
                archive_content(codename).as_bytes(),
                None,
            )
            .map_err(|err| SourcesError::Archive(Box::new(err)))
        }
    }
}

/// python-env's deadsnakes cleanup (host spec, "apt",
/// `remove_sources_mentioning`, amendment H3): each regular file directly
/// in `sources.list.d` whose bytes contain `needle` moved aside into
/// `/var/backups/sb/<folder>/<run id>/`, through the same fenced handles,
/// refusals and modes as [`apply`]'s cleanup, and each symlink whose
/// target mentions it, moved as a link ([`link_target`]: the target is
/// read through no other link, and never written or moved).
/// Subdirectories, other links and anything else are left alone; every
/// candidate is read before anything moves. Returns where each moved
/// entry now is, in name order. An empty `needle` names nothing, and a
/// missing `sources.list.d` holds nothing.
pub(crate) fn remove_mentioning(
    roots: &Roots,
    needle: &'static str,
    folder: &str,
    run_id: &str,
) -> Result<Vec<PathBuf>, SourcesError> {
    check_names(roots, folder, run_id)?;
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let sources_d = roots.etc_apt.join(SOURCES_D);
    let dir = match open_dir(&sources_d, roots.owner) {
        Ok(dir) => dir,
        Err(Opened::Missing) => return Ok(Vec::new()),
        Err(failed) => return Err(failed.error(&sources_d)),
    };
    let names = mentioning(&dir, &sources_d, needle.as_bytes())?;
    let mut report = Report::default();
    move_aside(roots, &dir, &sources_d, folder, run_id, &names, &mut report)?;
    // Built from the raw names, so a name that isn't UTF-8 is exact.
    let backup = backup_path(roots, folder, run_id);
    Ok(names
        .iter()
        .map(|(raw, _)| backup.join(OsStr::from_bytes(raw.to_bytes())))
        .collect())
}

/// A run id is letters and digits; a folder is a single plain name.
/// Either otherwise is refused before anything is looked at.
fn check_names(roots: &Roots, folder: &str, run_id: &str) -> Result<(), SourcesError> {
    if folder.is_empty() || folder == "." || folder == ".." || folder.contains('/') {
        return Err(SourcesError::Refused {
            dir: roots.var_backups.join(BACKUP_ROOT),
            reason: format!("{folder:?} is not a folder name"),
        });
    }
    if run_id.is_empty() || !run_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(SourcesError::Refused {
            dir: backup_path(roots, folder, ""),
            reason: format!("{run_id:?} is not a run id"),
        });
    }
    Ok(())
}

/// `/var/backups/sb/<folder>/<run id>`, or the folder for an empty run id.
fn backup_path(roots: &Roots, folder: &str, run_id: &str) -> PathBuf {
    let base = roots.var_backups.join(BACKUP_ROOT).join(folder);
    if run_id.is_empty() {
        base
    } else {
        base.join(run_id)
    }
}

/// Moves the entries of `dir` (`sources_d`; all but `ubuntu.sources` on
/// DEB822) into the run's backup directory, through directory handles (ops
/// spec, decisions 5 and 28), each recorded in `report` as it moves.
fn clean(
    roots: &Roots,
    dir: &OwnedFd,
    sources_d: &Path,
    release: Release,
    run_id: &str,
    report: &mut Report,
) -> Result<(), SourcesError> {
    let names = entries(dir, sources_d, release)?;
    move_aside(roots, dir, sources_d, APT_SOURCES, run_id, &names, report)
}

/// `names` from `dir` (`sources_d`) into `/var/backups/sb/<folder>/<run
/// id>/`, made only when there is something to move, each recorded in
/// `report` as it moves.
fn move_aside(
    roots: &Roots,
    dir: &OwnedFd,
    sources_d: &Path,
    folder: &str,
    run_id: &str,
    names: &[(CString, String)],
    report: &mut Report,
) -> Result<(), SourcesError> {
    if names.is_empty() {
        return Ok(());
    }
    // Evidence before acting: every name in the run log first, escaped so a
    // control character in a name can't forge a log line.
    for (_, name) in names {
        tracing::info!(
            name = %name.escape_debug(),
            dir = %sources_d.display(),
            "apt source to move aside"
        );
    }
    let backup = backup_path(roots, folder, run_id);
    let into = backup_dir(roots, folder, run_id)?;
    report.backup = Some(backup.clone());
    for (raw, name) in names {
        move_entry(roots.rename, dir.as_fd(), into.as_fd(), raw).map_err(|cause| {
            SourcesError::Move {
                path: sources_d.join(name),
                backup: backup.clone(),
                cause,
            }
        })?;
        report.moved.push(name.clone());
        tracing::info!(
            name = %name.escape_debug(),
            backup = %backup.display(),
            "apt source moved aside"
        );
    }
    // Both directories' entries on disk: the backup's new names, and the
    // removals from sources.list.d.
    sync_dir(&into, &backup)?;
    sync_dir(dir, sources_d)
}

/// `fsync` on a directory handle, so its entries are on disk.
fn sync_dir(dir: &OwnedFd, path: &Path) -> Result<(), SourcesError> {
    rustix::fs::fsync(dir).map_err(|cause| SourcesError::Write {
        path: path.to_path_buf(),
        cause: cause.into(),
    })
}

/// The entries to move, in name order, each `lstat`ed through the handle:
/// a regular file or a symlink; a subdirectory or anything else fails
/// before anything moves.
fn entries(
    dir: &OwnedFd,
    path: &Path,
    release: Release,
) -> Result<Vec<(CString, String)>, SourcesError> {
    let read = |cause: rustix::io::Errno| SourcesError::Read {
        path: path.to_path_buf(),
        cause: cause.into(),
    };
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(dir).map_err(read)? {
        let raw = entry.map_err(read)?.file_name().to_owned();
        let name = raw.to_string_lossy().into_owned();
        if name == "." || name == ".." || (release == Release::Deb822 && name == KEPT) {
            continue;
        }
        let stat =
            rustix::fs::statat(dir, raw.as_c_str(), AtFlags::SYMLINK_NOFOLLOW).map_err(read)?;
        let refused = |cause: io::Error| SourcesError::Remove {
            path: path.join(&name),
            cause,
        };
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile | FileType::Symlink => {}
            FileType::Directory => return Err(refused(rustix::io::Errno::ISDIR.into())),
            _ => {
                return Err(refused(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a regular file or symlink",
                )));
            }
        }
        names.push((raw, name));
    }
    names.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(names)
}

/// The entries directly in `dir` (`path`) apt reads whose bytes contain
/// `needle`, in name order, each `lstat`ed through the handle: a regular
/// file, read through the handle without following a link or waiting on
/// a FIFO (the type checked again on the opened file); or a symlink whose
/// target is one ([`link_target`]). Anything else is left alone.
fn mentioning(
    dir: &OwnedFd,
    path: &Path,
    needle: &[u8],
) -> Result<Vec<(CString, String)>, SourcesError> {
    let failed = |path: PathBuf, cause: io::Error| SourcesError::Read { path, cause };
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(dir).map_err(|err| failed(path.into(), err.into()))? {
        let raw = entry
            .map_err(|err| failed(path.into(), err.into()))?
            .file_name()
            .to_owned();
        let name = raw.to_string_lossy().into_owned();
        let stat = rustix::fs::statat(dir, raw.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|err| failed(path.join(&name), err.into()))?;
        let bytes = match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => {
                rustix::fs::openat(dir, raw.as_c_str(), file_flags(), Mode::empty())
                    .map_err(io::Error::from)
                    .and_then(read_regular)
                    .map(|(bytes, _)| Some(bytes))
            }
            FileType::Symlink => link_target(dir, raw.as_c_str()),
            _ => Ok(None),
        }
        .map_err(|cause| failed(path.join(&name), cause))?;
        if bytes.is_some_and(|bytes| contains(&bytes, needle)) {
            names.push((raw, name));
        }
    }
    names.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(names)
}

/// The bytes of the symlink `name`'s target in `dir`, read without
/// following any link beyond the entry itself (host spec, H3, amended
/// 2026-11-15). The link is read with `readlinkat`, its target opened
/// `O_PATH | O_NOFOLLOW` through the handle, which opens nothing (no
/// device or FIFO side effect) and refers to a final link itself
/// (open(2), `O_PATH`:
/// <https://man7.org/linux/man-pages/man2/open.2.html>), then `fstat`ed;
/// only a regular file is reopened read-only through that handle's
/// `/proc/self/fd` link, so the inode checked is the one read (`proc_pid_fd(5)`:
/// <https://man7.org/linux/man-pages/man5/proc_pid_fd.5.html>), and at
/// most [`MAX_LINKED`] bytes of it. A relative target resolves against
/// `dir`, as the kernel resolves a link for apt: "a relative pathname is
/// interpreted relative to the directory referred to by dirfd" (openat(2),
/// <https://man7.org/linux/man-pages/man2/openat.2.html>), and a relative
/// link "is interpreted relative to the directory containing the link"
/// (symlink(7), <https://man7.org/linux/man-pages/man7/symlink.7.html>).
/// `O_NOFOLLOW` binds the last component only; a directory link on the way
/// resolves as it does for apt, and nothing here is written. `None` for a
/// target that is missing (a dangling link), unreachable, another link, or
/// not a regular file: the entry is left alone.
fn link_target(dir: &OwnedFd, name: &CStr) -> io::Result<Option<Vec<u8>>> {
    let target = rustix::fs::readlinkat(dir, name, Vec::new())?;
    let shown = |cause: io::Error| {
        let target = target.to_string_lossy();
        io::Error::new(
            cause.kind(),
            format!("its target {}: {cause}", target.escape_debug()),
        )
    };
    // `file_flags()` with `O_PATH`, which keeps only `O_NOFOLLOW` and
    // `O_CLOEXEC` of them (open(2)).
    let flags = file_flags().union(OFlags::PATH);
    let fd = match rustix::fs::openat(dir, target.as_c_str(), flags, Mode::empty()) {
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => {
            return Ok(None);
        }
        opened => opened.map_err(|err| shown(err.into()))?,
    };
    if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
        return Ok(None);
    }
    let through = format!("/proc/self/fd/{}", fd.as_raw_fd());
    // `file_flags()` but `O_NOFOLLOW`: the /proc link is the one followed,
    // to that inode.
    let flags = file_flags().difference(OFlags::NOFOLLOW);
    let file = rustix::fs::open(through.as_str(), flags, Mode::empty())?;
    // `take` first, so a huge (or sparse) target is never reserved whole,
    // as `File::read_to_end` would from its size.
    let mut bytes = Vec::new();
    std::fs::File::from(file)
        .take(MAX_LINKED as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_LINKED {
        return Err(shown(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {MAX_LINKED} bytes"),
        )));
    }
    Ok(Some(bytes))
}

/// Why a directory couldn't be used.
enum Opened {
    Missing,
    Refused(String),
    Failed(io::Error),
}

impl Opened {
    fn error(self, dir: &Path) -> SourcesError {
        match self {
            Self::Missing => SourcesError::Refused {
                dir: dir.to_path_buf(),
                reason: "it is missing".to_string(),
            },
            Self::Refused(reason) => SourcesError::Refused {
                dir: dir.to_path_buf(),
                reason,
            },
            Self::Failed(cause) => SourcesError::Read {
                path: dir.to_path_buf(),
                cause,
            },
        }
    }
}

/// Flags for a directory handle that never follows a final symlink.
fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Flags for a file read through a handle: never a final symlink, and a
/// FIFO opened without waiting for a writer.
fn file_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}

/// The directory at `path`, opened without following and checked.
fn open_dir(path: &Path, owner: u32) -> Result<OwnedFd, Opened> {
    let opened = rustix::fs::open(path, dir_flags(), Mode::empty());
    checked(opened, path, owner)
}

/// `name` under `parent`, created 0700 when missing (the parent synced),
/// opened without following and checked.
fn ensure_dir(
    parent: &OwnedFd,
    path: &Path,
    name: &str,
    owner: u32,
) -> Result<OwnedFd, SourcesError> {
    match rustix::fs::mkdirat(parent, name, Mode::RWXU) {
        Ok(()) => sync_dir(parent, path.parent().unwrap_or(path))?,
        Err(rustix::io::Errno::EXIST) => {}
        Err(cause) => return Err(Opened::Failed(cause.into()).error(path)),
    }
    let opened = rustix::fs::openat(parent, name, dir_flags(), Mode::empty());
    checked(opened, path, owner).map_err(|failed| failed.error(path))
}

/// An opened directory refused unless it is `owner`'s and not writable by
/// group or others (ops spec, "The root is what it claims").
fn checked(
    opened: rustix::io::Result<OwnedFd>,
    path: &Path,
    owner: u32,
) -> Result<OwnedFd, Opened> {
    let fd = match opened {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Err(Opened::Missing),
        Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
            let symlink = path
                .symlink_metadata()
                .is_ok_and(|meta| meta.file_type().is_symlink());
            let reason = if symlink {
                "it is a symlink"
            } else {
                "not a directory"
            };
            return Err(Opened::Refused(reason.to_string()));
        }
        Err(cause) => return Err(Opened::Failed(cause.into())),
    };
    let stat = rustix::fs::fstat(&fd).map_err(|cause| Opened::Failed(cause.into()))?;
    if stat.st_uid != owner {
        return Err(Opened::Refused("not owned by root".to_string()));
    }
    if Mode::from_raw_mode(stat.st_mode).intersects(Mode::WGRP | Mode::WOTH) {
        return Err(Opened::Refused("writable by group or others".to_string()));
    }
    Ok(fd)
}

/// `/var/backups/sb/<folder>/<run id>`: `/var/backups` checked like the
/// others, the two folders created 0700 when missing and checked, the
/// run's created 0700, exclusively, each new one's parent synced.
fn backup_dir(roots: &Roots, folder: &str, run_id: &str) -> Result<OwnedFd, SourcesError> {
    let base = open_dir(&roots.var_backups, roots.owner)
        .map_err(|failed| failed.error(&roots.var_backups))?;
    let sb_path = roots.var_backups.join(BACKUP_ROOT);
    let sb = ensure_dir(&base, &sb_path, BACKUP_ROOT, roots.owner)?;
    let sources_path = sb_path.join(folder);
    let sources = ensure_dir(&sb, &sources_path, folder, roots.owner)?;
    let run_path = sources_path.join(run_id);
    match rustix::fs::mkdirat(&sources, run_id, Mode::RWXU) {
        Ok(()) => sync_dir(&sources, &sources_path)?,
        Err(rustix::io::Errno::EXIST) => {
            return Err(Opened::Refused("it already exists".to_string()).error(&run_path));
        }
        Err(cause) => return Err(Opened::Failed(cause.into()).error(&run_path)),
    }
    let opened = rustix::fs::openat(&sources, run_id, dir_flags(), Mode::empty());
    checked(opened, &run_path, roots.owner).map_err(|failed| failed.error(&run_path))
}

/// `name` from `from` into `to` with `renameat`; across filesystems
/// (`EXDEV`) copied, the backup directory synced, then unlinked through
/// the handle if it is still the entry that was copied.
fn move_entry(
    rename: Rename,
    from: BorrowedFd<'_>,
    to: BorrowedFd<'_>,
    name: &CStr,
) -> io::Result<()> {
    match rename(from, name, to, name) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::XDEV) => {
            let copied = copy_entry(from, to, name)?;
            rustix::fs::fsync(to)?;
            let now = rustix::fs::statat(from, name, AtFlags::SYMLINK_NOFOLLOW)?;
            if (now.st_dev, now.st_ino) != copied {
                return Err(io::Error::other("it changed while it was copied"));
            }
            rustix::fs::unlinkat(from, name, AtFlags::empty())?;
            Ok(())
        }
        Err(cause) => Err(cause.into()),
    }
}

/// One entry copied between handles, never following a link: a symlink as
/// a symlink, a regular file's bytes and mode (without setuid and setgid),
/// synced. Returns the source's device and inode, as `lstat`ed before the
/// copy. Why not `std::fs::copy` or `fs_extra`: both work on paths, which a
/// swapped link could redirect; `io::copy` between the two handles uses
/// `copy_file_range` on Linux.
fn copy_entry(from: BorrowedFd<'_>, to: BorrowedFd<'_>, name: &CStr) -> io::Result<(u64, u64)> {
    let stat = rustix::fs::statat(from, name, AtFlags::SYMLINK_NOFOLLOW)?;
    let id = (stat.st_dev, stat.st_ino);
    if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink {
        let target = rustix::fs::readlinkat(from, name, Vec::new())?;
        rustix::fs::symlinkat(target.as_c_str(), to, name)?;
        return Ok(id);
    }
    let mode = Mode::from_raw_mode(stat.st_mode) & !(Mode::SUID | Mode::SGID);
    let source = rustix::fs::openat(from, name, file_flags(), Mode::empty())?;
    let copy = rustix::fs::openat(
        to,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode,
    )?;
    // The mode as it was, whatever the umask.
    rustix::fs::fchmod(&copy, mode)?;
    let mut copy = std::fs::File::from(copy);
    io::copy(&mut std::fs::File::from(source), &mut copy)?;
    copy.sync_all()?;
    Ok(id)
}

/// A file's mode and owner, kept when it is saved again.
#[derive(Debug, Clone, Copy)]
struct Owner {
    mode: u32,
    uid: u32,
    gid: u32,
}

/// `sources.list` as read before anything moves: where it is saved (a
/// symlink's target), its bytes, and its mode and owner, all from one
/// handle.
struct Existing {
    dir: PathBuf,
    name: OsString,
    path: PathBuf,
    bytes: Vec<u8>,
    owner: Option<Owner>,
}

/// `sources.list` read once: a symlink resolved to its target, which is
/// opened without following; its bytes, mode and owner from that handle. A
/// missing file is empty; a dangling link fails here, before anything
/// moves.
fn read_sources_list(path: &Path) -> Result<Existing, SourcesError> {
    let failed = |path: &Path, cause: io::Error| SourcesError::Read {
        path: path.to_path_buf(),
        cause,
    };
    let target = match path.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            path.canonicalize().map_err(|cause| failed(path, cause))?
        }
        Ok(_) => path.to_path_buf(),
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => {
            return Ok(Existing {
                dir: parent(path),
                name: path.file_name().unwrap_or_default().to_owned(),
                path: path.to_path_buf(),
                bytes: Vec::new(),
                owner: None,
            });
        }
        Err(cause) => return Err(failed(path, cause)),
    };
    let fd = rustix::fs::open(&target, file_flags(), Mode::empty())
        .map_err(|cause| failed(&target, cause.into()))?;
    let (bytes, stat) = read_regular(fd).map_err(|cause| failed(&target, cause))?;
    Ok(Existing {
        dir: parent(&target),
        name: target.file_name().unwrap_or_default().to_owned(),
        bytes,
        owner: Some(Owner {
            mode: stat.st_mode & 0o7777,
            uid: stat.st_uid,
            gid: stat.st_gid,
        }),
        path: target,
    })
}

/// `path`'s directory.
fn parent(path: &Path) -> PathBuf {
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// A regular file's bytes and `fstat` through its handle; anything else
/// (a FIFO, a device) fails.
fn read_regular(fd: OwnedFd) -> io::Result<(Vec<u8>, rustix::fs::Stat)> {
    let stat = rustix::fs::fstat(&fd)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::from(fd).read_to_end(&mut bytes)?;
    Ok((bytes, stat))
}

/// `existing`'s lines trimmed, without blanks and repeats, then each of
/// the four archive lines not already there (sb-go's `addRepo`), on bytes,
/// so a line that isn't UTF-8 is kept as it is.
fn legacy_lines(existing: &[u8], codename: &str) -> Vec<u8> {
    let wanted = ["main", "universe", "restricted", "multiverse"]
        .map(|component| format!("deb http://archive.ubuntu.com/ubuntu/ {codename} {component}"));
    let mut lines: Vec<&[u8]> = Vec::new();
    for line in existing
        .split(|&byte| byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .chain(wanted.iter().map(String::as_bytes))
    {
        if !line.is_empty() && !lines.contains(&line) {
            lines.push(line);
        }
    }
    let mut text = lines.join(&b'\n');
    text.push(b'\n');
    text
}

/// sb-go's `isUsingArchiveMirror` on `ubuntu.sources`, read through `dir`
/// without following a link or waiting on a FIFO: a `URIs:` value naming
/// the official archive; a missing file or no URIs is a custom mirror.
fn uses_archive(dir: &OwnedFd, path: &Path) -> Result<bool, SourcesError> {
    let failed = |cause: io::Error| SourcesError::Read {
        path: path.to_path_buf(),
        cause,
    };
    let fd = match rustix::fs::openat(dir, KEPT, file_flags(), Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(cause) => return Err(failed(cause.into())),
    };
    let (bytes, _) = read_regular(fd).map_err(failed)?;
    Ok(bytes
        .split(|&byte| byte == b'\n')
        .filter_map(|line| line.trim_ascii().strip_prefix(b"URIs:"))
        .any(|uris| {
            contains(uris, b"archive.ubuntu.com") || contains(uris, b"security.ubuntu.com")
        }))
}

/// Whether `needle` (not empty: `windows(0)` panics) occurs in `haystack`.
/// Why not `memchr::memmem::find`: memchr is only a transitive dependency
/// here, and making it sb-host's own is a new dependency (host spec,
/// Boundaries, "ask first") for a scan of a few small files.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// sb-go's `buildUbuntuArchiveSourcesContent`, exactly.
fn archive_content(codename: &str) -> String {
    format!(
        "Types: deb\n\
         URIs: http://archive.ubuntu.com/ubuntu/\n\
         Suites: {codename} {codename}-updates {codename}-backports\n\
         Components: main restricted universe multiverse\n\
         Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\
         \n\
         Types: deb\n\
         URIs: http://security.ubuntu.com/ubuntu/\n\
         Suites: {codename}-security\n\
         Components: main restricted universe multiverse\n\
         Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"
    )
}

/// `content` saved as `name` in `dir` atomically through tempfile
/// (<https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist>):
/// a temporary file in the same directory under a name apt ignores
/// (`.<name>.<random>~`), `owner`'s mode and owner (a new file 0644),
/// synced, renamed over it, then the directory synced, which persist
/// leaves to the caller. `shown` names the file in errors. Why not
/// `Builder::permissions`: the umask narrows it, and tempfile has no owner
/// setting. sb-go truncated in place (difference 8).
fn save(
    dir: &Path,
    name: &OsStr,
    shown: &Path,
    content: &[u8],
    owner: Option<Owner>,
) -> Result<(), SourcesError> {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    let failed = |cause: io::Error| SourcesError::Write {
        path: shown.to_path_buf(),
        cause,
    };
    let mut file = tempfile::Builder::new()
        .prefix(&format!(".{}.", name.to_string_lossy()))
        .suffix(TEMP_SUFFIX)
        .tempfile_in(dir)
        .map_err(failed)?;
    file.write_all(content).map_err(failed)?;
    let mode = owner.map_or(0o644, |owner| owner.mode);
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(failed)?;
    if let Some(owner) = owner {
        std::os::unix::fs::fchown(file.as_file(), Some(owner.uid), Some(owner.gid))
            .map_err(failed)?;
    }
    file.as_file().sync_all().map_err(failed)?;
    file.persist(dir.join(name))
        .map_err(|err| failed(err.error))?;
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(failed)
}

/// `renameat` from rustix
/// (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.renameat.html>).
fn rename(
    from: BorrowedFd<'_>,
    name: &CStr,
    to: BorrowedFd<'_>,
    new: &CStr,
) -> rustix::io::Result<()> {
    rustix::fs::renameat(from, name, to, new)
}

#[cfg(test)]
mod tests;
