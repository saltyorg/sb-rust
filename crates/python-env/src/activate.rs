//! Generations on disk (python-env spec, steps 13 and 15; sb-go's
//! `python/activation.go`). So far the cleanup after a run: only the
//! active generation and its Python release are kept. The activation
//! itself is module build M16's.

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::Layout;
use crate::manifest::{self, ManifestError};

/// Why old generations couldn't be cleaned, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    #[error("inspect obsolete previous venv link: {0}")]
    InspectPrevious(io::Error),
    #[error("obsolete previous venv path {} is not a symlink", .0.display())]
    PreviousNotLink(PathBuf),
    #[error("remove obsolete previous venv link: {0}")]
    RemovePrevious(io::Error),
    #[error("resolve active generation {}: {cause}", path.display())]
    Resolve { path: PathBuf, cause: io::Error },
    #[error(
        "active generation {} is not directly under managed releases {}",
        generation.display(),
        releases.display()
    )]
    GenerationOutside {
        generation: PathBuf,
        releases: PathBuf,
    },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// The active manifest lacks a field (sb-go would read a zero value
    /// and fail on the Python release below).
    #[error("generation manifest {} is incomplete", .0.join(manifest::FILE).display())]
    Incomplete(PathBuf),
    #[error(
        "active Python installation {install} is not directly under managed releases {}",
        releases.display()
    )]
    PythonOutside { install: String, releases: PathBuf },
    #[error("read releases {}: {cause}", dir.display())]
    List { dir: PathBuf, cause: io::Error },
    #[error("remove unretained release {}: {cause}", path.display())]
    Remove { path: PathBuf, cause: io::Error },
}

/// Whether `path` is one entry directly inside `root`, compared by their
/// components as sb-go's `directChildOf` compares by `filepath.Rel`
/// (lexically: neither is resolved).
fn direct_child(path: &Path, root: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(root) else {
        return false;
    };
    let mut parts = rest.components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    )
}

/// Keep only the active generation and its Python release (sb-go's
/// `cleanupGenerations`): an obsolete `previous` link goes first; the
/// active link is resolved (std's `canonicalize`, as Go's
/// `filepath.EvalSymlinks`) and its generation must be directly under
/// `releases/`, its manifest naming a Python release directly under
/// Python's `releases/`; then every other entry of both `releases/`
/// directories is removed. Nothing outside the two is touched (Saltbox's
/// `python` role installs into `/srv/python` too).
///
/// # Errors
///
/// [`CleanupError`]: the first check or removal that fails; when a check
/// fails, nothing but the obsolete `previous` link (removed first, as
/// sb-go does) is gone.
pub fn cleanup_generations(layout: &Layout) -> Result<(), CleanupError> {
    remove_previous_link(&layout.ansible.join("previous"))?;
    let target =
        std::fs::canonicalize(&layout.active_venv).map_err(|cause| CleanupError::Resolve {
            path: layout.active_venv.clone(),
            cause,
        })?;
    let generation = target.parent().unwrap_or(&target).to_path_buf();
    if !direct_child(&generation, &layout.ansible_releases) {
        return Err(CleanupError::GenerationOutside {
            generation,
            releases: layout.ansible_releases.clone(),
        });
    }
    let active =
        manifest::read(&generation)?.ok_or_else(|| CleanupError::Incomplete(generation.clone()))?;
    let install = PathBuf::from(&active.python_install);
    if active.python_install.is_empty() || !direct_child(&install, &layout.python_releases) {
        return Err(CleanupError::PythonOutside {
            install: active.python_install,
            releases: layout.python_releases.clone(),
        });
    }
    remove_others(&layout.ansible_releases, &generation)?;
    remove_others(&layout.python_releases, &install)
}

/// Remove `path` when it is the obsolete `previous` symlink; a missing
/// one is fine, anything else there an error (sb-go's
/// `removeObsoletePreviousLink`).
fn remove_previous_link(path: &Path) -> Result<(), CleanupError> {
    match std::fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(cause) => Err(CleanupError::InspectPrevious(cause)),
        Ok(meta) if !meta.file_type().is_symlink() => {
            Err(CleanupError::PreviousNotLink(path.to_path_buf()))
        }
        Ok(_) => std::fs::remove_file(path).map_err(CleanupError::RemovePrevious),
    }
}

/// Remove every entry of `root` but `keep` (sb-go's
/// `removeUnretainedDirectories`); a missing `root` has none. A symlink
/// entry is removed itself, never followed.
fn remove_others(root: &Path, keep: &Path) -> Result<(), CleanupError> {
    let entries = match std::fs::read_dir(root) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(cause) => {
            return Err(CleanupError::List {
                dir: root.to_path_buf(),
                cause,
            });
        }
        Ok(entries) => entries,
    };
    for entry in entries {
        let entry = entry.map_err(|cause| CleanupError::List {
            dir: root.to_path_buf(),
            cause,
        })?;
        let path = entry.path();
        if path == keep {
            continue;
        }
        let removed = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(cause) => Err(cause),
        };
        removed.map_err(|cause| CleanupError::Remove { path, cause })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_direct_child_is_one_entry_down() {
        let root = Path::new("/srv/ansible/releases");
        assert!(direct_child(Path::new("/srv/ansible/releases/gen"), root));
        for not in [
            "/srv/ansible/releases",
            "/srv/ansible/releases/gen/venv",
            "/srv/ansible/other/gen",
            "/srv/ansible/releases/../gen",
            "/elsewhere",
        ] {
            assert!(!direct_child(Path::new(not), root), "{not}");
        }
    }

    /// sb-go's `TestRemoveObsoletePreviousLinkRejectsDirectory`.
    #[test]
    fn a_previous_directory_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("previous");
        std::fs::create_dir(&path).expect("dir");
        let err = remove_previous_link(&path).expect_err("a directory");
        assert_eq!(
            err.to_string(),
            format!(
                "obsolete previous venv path {} is not a symlink",
                path.display()
            )
        );
        assert!(path.is_dir());
        assert!(remove_previous_link(&dir.path().join("absent")).is_ok());
    }
}
