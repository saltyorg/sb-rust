//! Activation (release spec, "Activation"): a verified staged file put in
//! place of its target atomically, with its mode, the directory synced.
//! sb-go's `Activate` (release/asset.go) is the reference for the steps
//! and the texts. Why not `self-replace`: it copies once more and doesn't
//! sync the directory; tempfile's `persist` is the rename itself.

use std::fs::{File, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tempfile::TempPath;

use crate::Error;

/// Put `staged` in place of `target` with `mode`: chmod, a rename over
/// `target` (tempfile's `persist`,
/// <https://docs.rs/tempfile/3.27.0/tempfile/struct.TempPath.html#method.persist>),
/// then an fsync of the directory so the rename survives a crash. `staged`
/// must be in `target`'s directory (else an error before anything
/// changes): a [`Staged`](crate::Staged) download, or a file the caller
/// staged itself (`NamedTempFile::into_temp_path`), the directory
/// compared as written (another spelling of it is refused, safely), and
/// writable by root only. If anything fails before the rename, `staged` is
/// removed and `target` is as it was; a `sync release asset directory:`
/// error comes after it, with the new file already in place.
///
/// # Errors
///
/// [`Error::Activate`] with sb-go's texts (the directory's open and sync
/// in one).
pub fn activate(staged: impl Into<TempPath>, target: &Path, mode: u32) -> Result<(), Error> {
    let staged = staged.into();
    let dir = directory(target);
    if directory(&staged) != dir {
        return Err(Error::Activate(
            "staged release asset must be in the target directory".to_owned(),
        ));
    }
    std::fs::set_permissions(&staged, Permissions::from_mode(mode))
        .map_err(io_failed("set staged release asset permissions"))?;
    staged
        .persist(target)
        .map_err(|err| io_failed("activate release asset")(err.error))?;
    // A directory opened read-only can be synced on Linux
    // (<https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_all>).
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(io_failed("sync release asset directory"))
}

/// `path`'s directory, `.` for a bare file name.
fn directory(path: &Path) -> &Path {
    path.parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// An I/O failure while activating, after `what`.
fn io_failed(what: &'static str) -> impl FnOnce(std::io::Error) -> Error {
    move |err| Error::Activate(format!("{what}: {err}"))
}
