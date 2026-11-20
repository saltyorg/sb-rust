//! A missing `hetzner_vlan.yml` or `providers.yml` copied in from
//! Saltbox's default (config spec, Behaviour, "A missing
//! `hetzner_vlan.yml` or `providers.yml`"; decision 10): Saltbox's playbook reads
//! `defaults/<file>.default` in the file's place (saltbox.yml's
//! `vars_files`), and sb makes the file the user edits.
//!
//! Library-backed: every file step goes through rustix on handles opened
//! without following links, as motd.yml's migration does (`openat2`,
//! `openat`, `statat`, `linkat`, `unlinkat`, `fsync`;
//! <https://docs.rs/rustix/1.1.6/rustix/fs/fn.linkat.html>), and std's
//! `fchown` and `File::set_permissions` on the new file's handle
//! (<https://doc.rust-lang.org/std/os/unix/fs/fn.fchown.html>). Why not
//! ops' `copy_default_configs` (sb setup's copy): it lives in sb-ops,
//! which sb-config can't depend on (the capability map), writes in place
//! by path and keeps root as the owner; this copy keeps the default's
//! owner. Why `linkat` over a rename with `RENAME_NOREPLACE`: both never
//! replace a file, and `linkat` works on every local filesystem, where
//! `renameat2`'s flag doesn't (ZFS before 2.2). Why not tempfile's
//! `persist_noclobber`: it works by path, where root must stay on the
//! folder's handle (the Saltbox user owns the checkout).

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;
use sb_cli_core::config::MAX_FILE_BYTES;

use crate::migrate::open_folder;
use crate::run::File;

/// How the default is opened: read-only, never blocking (a FIFO can't
/// hang sb), not kept across an exec, never made the controlling
/// terminal. A link isn't followed anywhere on its path, the last part
/// included ([`READ_RESOLVE`]'s `NO_SYMLINKS`). Constants, as
/// `facts::session`'s flags.
const READ_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOCTTY);

/// How the default's path is resolved under the root: no link, no mount
/// point crossed (a FUSE mount there could hang root's run, cli-core's
/// `read_beneath` rule), never above the root.
const READ_RESOLVE: ResolveFlags = ResolveFlags::NO_SYMLINKS
    .union(ResolveFlags::NO_XDEV)
    .union(ResolveFlags::BENEATH);

/// How the temporary file is made: new (`O_EXCL`, so never through a
/// link), write-only, not kept across an exec.
const CREATE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// How the copy of a file's default went.
#[derive(Debug, PartialEq, Eq)]
pub enum CopyOutcome {
    /// Nothing to do: the file has no default entry, something is at its
    /// path (a link included), or the root isn't there (the file's check
    /// reports it).
    NotNeeded,
    /// The default was copied in.
    Copied,
    /// There is no default to copy (the missing file stays its issue).
    NoDefault,
    /// The copy failed, for this reason; nothing is left behind.
    Failed(String),
}

/// `file`'s default copied to its path under `root` when nothing is
/// there: the default's bytes (a regular file, read up to cli-core's
/// 1 MiB through a handle that follows no link), written to a new
/// temporary file beside the file, given the default's owner and group
/// and mode 0600, synced, then linked to the file's name, which never
/// replaces anything; the temporary name removed and the folder synced.
/// A file that appears meanwhile is kept ([`CopyOutcome::NotNeeded`]).
/// A root reached through a link isn't written in (motd.yml's
/// migration's rule): the copy fails, saying why. Logged.
#[must_use]
pub fn copy_default(root: &Path, file: File) -> CopyOutcome {
    let Some(default) = file.default else {
        return CopyOutcome::NotNeeded;
    };
    // In a build for tests, never the host's checkout (as motd.yml's
    // migration: a mutant dropping `SB_SALTBOX_DIR` points there).
    if cfg!(any(test, feature = "testing")) && root.starts_with("/srv") {
        return CopyOutcome::Failed("a test build doesn't write under /srv".to_string());
    }
    let dir = match open_folder(root) {
        Ok(dir) => dir,
        Err(Errno::NOENT) => return CopyOutcome::NotNeeded,
        Err(errno) => {
            let why = format!(
                "the Saltbox root can't be opened without following a link: {}",
                std::io::Error::from(errno)
            );
            tracing::warn!(file = file.path, default, error = %why, "missing config file not copied from Saltbox's default");
            return CopyOutcome::Failed(why);
        }
    };
    match rustix::fs::statat(&dir, file.path, AtFlags::SYMLINK_NOFOLLOW) {
        Err(Errno::NOENT) => {}
        _ => return CopyOutcome::NotNeeded,
    }
    let copy = match read_default(&dir, default) {
        Ok(Some(found)) => match write_new(&dir, file.path, &found.bytes, found.owner) {
            Ok(true) => CopyOutcome::Copied,
            Ok(false) => CopyOutcome::NotNeeded,
            Err(err) => CopyOutcome::Failed(err.to_string()),
        },
        Ok(None) => CopyOutcome::NoDefault,
        Err(err) => CopyOutcome::Failed(err.to_string()),
    };
    match &copy {
        CopyOutcome::Copied => {
            tracing::info!(
                file = file.path,
                default,
                "missing config file copied from Saltbox's default"
            );
        }
        CopyOutcome::Failed(error) => tracing::warn!(
            file = file.path,
            default,
            error = %error,
            "missing config file not copied from Saltbox's default"
        ),
        CopyOutcome::NoDefault => tracing::info!(
            file = file.path,
            default,
            "missing config file has no default to copy"
        ),
        CopyOutcome::NotNeeded => {}
    }
    copy
}

/// A default's bytes and its owner and group.
struct Default {
    bytes: Vec<u8>,
    owner: (u32, u32),
}

/// The default at `default` under `dir`, none when it isn't there.
/// Opened with no link on its path, no mount point crossed (a FUSE mount
/// there could hang root's run, cli-core's `read_beneath` rule) and
/// without blocking (a FIFO can't hang sb), checked to be a regular
/// file, read up to cli-core's cap.
fn read_default(dir: &OwnedFd, default: &str) -> std::io::Result<Option<Default>> {
    let fd = match rustix::fs::openat2(dir, default, READ_FLAGS, Mode::empty(), READ_RESOLVE) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(errno) => return Err(errno.into()),
    };
    let stat = rustix::fs::fstat(&fd)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(std::io::Error::other(format!(
            "{default} is not a regular file"
        )));
    }
    let mut bytes = Vec::new();
    let cap = u64::try_from(MAX_FILE_BYTES).unwrap_or(u64::MAX);
    fs::File::from(fd)
        .take(cap.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(std::io::Error::other(format!(
            "{default} is larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    Ok(Some(Default {
        bytes,
        owner: (stat.st_uid, stat.st_gid),
    }))
}

/// `bytes` written as a new file `name` in `dir`, owned by `owner`, mode
/// 0600: `false` when a file appeared at `name` meanwhile (kept as it
/// is). The temporary file is removed on every path but a crash between
/// its creation and its removal (it then holds the default's bytes,
/// 0600). A folder sync that fails after the link is logged: the file
/// is in place.
fn write_new(dir: &OwnedFd, name: &str, bytes: &[u8], owner: (u32, u32)) -> std::io::Result<bool> {
    let temporary = format!(
        ".{name}.{}.{:016x}.sb-tmp",
        std::process::id(),
        fastrand::u64(..)
    );
    let fd = rustix::fs::openat(
        dir,
        temporary.as_str(),
        CREATE_FLAGS,
        Mode::from_raw_mode(0o600),
    )?;
    let mut file = fs::File::from(fd);
    let written = file
        .write_all(bytes)
        .and_then(|()| std::os::unix::fs::fchown(&file, Some(owner.0), Some(owner.1)))
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(0o600)))
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            match rustix::fs::linkat(dir, temporary.as_str(), dir, name, AtFlags::empty()) {
                Ok(()) => Ok(true),
                Err(Errno::EXIST) => Ok(false),
                Err(errno) => Err(errno.into()),
            }
        });
    drop(file);
    let _ = rustix::fs::unlinkat(dir, temporary.as_str(), AtFlags::empty());
    let linked = written?;
    if linked && let Err(errno) = rustix::fs::fsync(dir) {
        tracing::warn!(error = %std::io::Error::from(errno), "Saltbox root not synced after the default copy");
    }
    Ok(linked)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt as _;

    use super::*;
    use crate::run::FILES;

    fn providers() -> File {
        *FILES
            .iter()
            .find(|file| file.path == "providers.yml")
            .expect("providers.yml")
    }

    fn with_default(text: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("defaults")).expect("defaults");
        std::fs::write(root.path().join("defaults/providers.yml.default"), text).expect("default");
        root
    }

    #[test]
    fn a_missing_file_is_copied_0600() {
        let root = with_default("duckdns:\n  token:\n");
        assert_eq!(copy_default(root.path(), providers()), CopyOutcome::Copied);
        let path = root.path().join("providers.yml");
        assert_eq!(
            std::fs::read_to_string(&path).expect("copy"),
            "duckdns:\n  token:\n"
        );
        let meta = std::fs::metadata(&path).expect("meta");
        assert_eq!(meta.mode() & 0o7777, 0o600);
        assert_eq!(meta.nlink(), 1, "the temporary name is gone");
    }

    #[test]
    fn a_present_file_is_left_alone() {
        let root = with_default("default\n");
        std::fs::write(root.path().join("providers.yml"), "mine\n").expect("mine");
        assert_eq!(
            copy_default(root.path(), providers()),
            CopyOutcome::NotNeeded
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("providers.yml")).expect("mine"),
            "mine\n"
        );
    }

    #[test]
    fn a_file_without_a_default_entry_is_left_alone() {
        let root = with_default("default\n");
        let settings = *FILES
            .iter()
            .find(|file| file.path == "settings.yml")
            .expect("settings.yml");
        assert_eq!(copy_default(root.path(), settings), CopyOutcome::NotNeeded);
        assert!(!root.path().join("settings.yml").exists());
    }

    /// A Saltbox root that isn't there needs no copy: the file's check
    /// reports it missing.
    #[test]
    fn a_missing_root_needs_no_copy() {
        let root = tempfile::tempdir().expect("root");
        let gone = root.path().join("gone");
        assert_eq!(copy_default(&gone, providers()), CopyOutcome::NotNeeded);
        assert!(!gone.exists());
    }

    #[test]
    fn a_missing_default_copies_nothing() {
        let root = tempfile::tempdir().expect("root");
        assert_eq!(
            copy_default(root.path(), providers()),
            CopyOutcome::NoDefault
        );
        assert!(!root.path().join("providers.yml").exists());
    }

    /// A file that appears between the look and the link is kept: the
    /// link never replaces it, and the temporary file goes.
    #[test]
    fn a_file_made_meanwhile_is_kept() {
        let root = tempfile::tempdir().expect("root");
        std::fs::write(root.path().join("providers.yml"), "theirs\n").expect("theirs");
        let dir = open_folder(root.path()).expect("dir");
        assert!(!write_new(&dir, "providers.yml", b"ours\n", (0, 0)).expect("write"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("providers.yml")).expect("theirs"),
            "theirs\n"
        );
        assert_eq!(std::fs::read_dir(root.path()).expect("dir").count(), 1);
    }

    /// A default of exactly cli-core's cap is copied whole.
    #[test]
    fn a_default_at_the_cap_is_copied() {
        let text = "x".repeat(MAX_FILE_BYTES);
        let root = with_default(&text);
        assert_eq!(copy_default(root.path(), providers()), CopyOutcome::Copied);
        assert_eq!(
            std::fs::read_to_string(root.path().join("providers.yml")).expect("copy"),
            text
        );
    }

    /// A default over the cap, or not a regular file, or reached through
    /// a link, is refused with its reason, and nothing is copied.
    #[test]
    fn a_default_that_cant_be_read_fails() {
        let root = with_default(&"x".repeat(MAX_FILE_BYTES + 1));
        let CopyOutcome::Failed(why) = copy_default(root.path(), providers()) else {
            panic!("not refused");
        };
        assert!(why.contains("larger than"), "{why}");
        assert!(!root.path().join("providers.yml").exists());
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir_all(root.path().join("defaults/providers.yml.default")).expect("dir");
        let CopyOutcome::Failed(why) = copy_default(root.path(), providers()) else {
            panic!("not refused");
        };
        assert!(why.contains("not a regular file"), "{why}");
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("defaults")).expect("defaults");
        std::fs::write(root.path().join("real.default"), "x\n").expect("real");
        std::os::unix::fs::symlink(
            root.path().join("real.default"),
            root.path().join("defaults/providers.yml.default"),
        )
        .expect("link");
        assert!(matches!(
            copy_default(root.path(), providers()),
            CopyOutcome::Failed(_)
        ));
        assert!(!root.path().join("providers.yml").exists());
    }

    /// A FIFO as the default is refused at once: opened without
    /// blocking, then not a regular file.
    #[test]
    fn a_fifo_default_is_refused_without_waiting() {
        let root = tempfile::tempdir().expect("root");
        std::fs::create_dir(root.path().join("defaults")).expect("defaults");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            root.path().join("defaults/providers.yml.default"),
            FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .expect("fifo");
        let CopyOutcome::Failed(why) = copy_default(root.path(), providers()) else {
            panic!("not refused");
        };
        assert!(why.contains("not a regular file"), "{why}");
        assert!(!root.path().join("providers.yml").exists());
    }

    #[test]
    fn a_test_build_never_writes_under_srv() {
        assert!(matches!(
            copy_default(Path::new("/srv/git/saltbox"), providers()),
            CopyOutcome::Failed(_)
        ));
    }
}
