//! Generations on disk (python-env spec, steps 13 and 15; sb-go's
//! `python/activation.go`): a staged generation's activation by symlink
//! swap, a legacy venv directory adopted on the way, and the cleanup
//! after a run, which keeps only the active generation and its Python
//! release.

use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Component, Path, PathBuf};

use tempfile::TempPath;

use crate::Layout;
use crate::manifest::{self, ManifestError};

/// Why a generation couldn't be activated, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum ActivateError {
    #[error("create Ansible releases directory: {0}")]
    Releases(io::Error),
    #[error("inspect active venv before switch: {0}")]
    Inspect(io::Error),
    #[error("create legacy generation: {0}")]
    LegacyDir(io::Error),
    #[error("adopt legacy venv: {0}")]
    Adopt(io::Error),
    #[error("activate Ansible venv: {0}")]
    Switch(io::Error),
}

/// Make `venv` the active venv (sb-go's `activateGenerationAt`): a legacy
/// directory at the active path (sb-go's layout before generations) moves
/// to `releases/legacy-<stamp>/venv` first; then a new symlink beside the
/// active path is renamed over it, so an active link names the old venv
/// or the new one at every moment (rename(2) replaces atomically,
/// <https://man7.org/linux/man-pages/man2/rename.2.html>; a legacy
/// directory leaves the path empty between its move and the rename). The temporary
/// link is tempfile's (`Builder::make_in`, which retries a taken name,
/// <https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.make_in>;
/// `TempPath::persist` renames it, and a failed one removes it,
/// <https://docs.rs/tempfile/3.27.0/tempfile/struct.TempPath.html#method.persist>).
/// If the switch fails, a moved legacy directory is put back.
///
/// # Errors
///
/// [`ActivateError`]: the first part that fails; the active path is then
/// as it was, unless putting a legacy directory back failed too (sb-go
/// ignores that; the run log keeps it, and the directory stays under
/// `releases/legacy-<stamp>/`).
pub fn activate_generation(layout: &Layout, venv: &Path) -> Result<(), ActivateError> {
    activate_with(layout, venv, |temporary, link| {
        temporary.persist(link).map_err(|err| err.error)
    })
}

/// [`activate_generation`] with the switch's rename given (a test's seam
/// for a rename that fails).
fn activate_with(
    layout: &Layout,
    venv: &Path,
    rename: impl FnOnce(TempPath, &Path) -> io::Result<()>,
) -> Result<(), ActivateError> {
    let dirs = || {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o755);
        builder
    };
    dirs()
        .create(&layout.ansible_releases)
        .map_err(ActivateError::Releases)?;
    let active = &layout.active_venv;
    let legacy = match found(std::fs::symlink_metadata(active))? {
        Found::Replaceable => None,
        Found::Legacy => {
            let id = format!("legacy-{}", crate::venv::stamp(jiff::Timestamp::now()));
            let dir = layout.ansible_releases.join(id);
            dirs().create(&dir).map_err(ActivateError::LegacyDir)?;
            let moved = dir.join("venv");
            std::fs::rename(active, &moved).map_err(ActivateError::Adopt)?;
            Some(moved)
        }
    };
    let switched = switch(active, venv, rename);
    if switched.is_err()
        && let Some(moved) = legacy
        && let Err(err) = std::fs::rename(&moved, active)
    {
        // sb-go ignores this error; the run log keeps it.
        tracing::warn!(%err, from = %moved.display(), "could not put the legacy venv back");
    }
    switched.map_err(ActivateError::Switch)
}

/// What the active path holds before a switch.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// Nothing, a link, or any other non-directory (a file there, which
    /// sb-go refuses; python-env spec, difference 17): the switch
    /// replaces it.
    Replaceable,
    /// A real directory: sb-go's legacy venv, adopted first.
    Legacy,
}

/// The active path as `lstat` found it (`meta`).
fn found(meta: io::Result<std::fs::Metadata>) -> Result<Found, ActivateError> {
    match meta {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Found::Replaceable),
        Err(err) => Err(ActivateError::Inspect(err)),
        Ok(meta) if meta.is_dir() => Ok(Found::Legacy),
        Ok(_) => Ok(Found::Replaceable),
    }
}

/// `link` replaced by a symlink to `target` (sb-go's `atomicSymlink`).
fn switch(
    link: &Path,
    target: &Path,
    rename: impl FnOnce(TempPath, &Path) -> io::Result<()>,
) -> io::Result<()> {
    // The layout's active path always has a parent (`/srv/ansible`).
    let dir = link.parent().unwrap_or(link);
    let temporary = tempfile::Builder::new()
        .prefix(".link-")
        .make_in(dir, |path| std::os::unix::fs::symlink(target, path))?;
    rename(temporary.into_temp_path(), link)
}

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
pub(crate) fn direct_child(path: &Path, root: &Path) -> bool {
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

    /// A tree with a staged venv `new` under `releases/`.
    fn staged() -> (tempfile::TempDir, Layout, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = Layout::under(dir.path());
        let venv = layout.ansible_releases.join("new/venv");
        std::fs::create_dir_all(&venv).expect("venv");
        (dir, layout, venv)
    }

    /// The names in the active path's directory but the active path, the
    /// releases and `previous`: a temporary link left behind shows here.
    fn strays(layout: &Layout) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&layout.ansible)
            .expect("ansible")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| !["venv", "releases", "previous"].contains(&name.as_str()))
            .collect();
        names.sort();
        names
    }

    /// The releases' entries, by name.
    fn releases(layout: &Layout) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&layout.ansible_releases)
            .expect("releases")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_first_activation_makes_the_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = Layout::under(dir.path());
        let venv = layout.ansible_releases.join("new/venv");
        // No `releases/` yet: made, 0755, with the active path's parent.
        activate_generation(&layout, &venv).expect("activated");
        assert_eq!(std::fs::read_link(&layout.active_venv).expect("link"), venv);
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&layout.ansible_releases)
                .expect("releases")
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o755);
        assert!(strays(&layout).is_empty(), "{:?}", strays(&layout));
    }

    #[test]
    fn an_active_link_is_replaced() {
        let (_dir, layout, venv) = staged();
        let old = layout.ansible_releases.join("old/venv");
        std::fs::create_dir_all(&old).expect("old");
        std::os::unix::fs::symlink(&old, &layout.active_venv).expect("link");
        activate_generation(&layout, &venv).expect("activated");
        assert_eq!(std::fs::read_link(&layout.active_venv).expect("link"), venv);
        assert!(old.is_dir(), "the old generation stays for the cleanup");
        assert!(strays(&layout).is_empty(), "{:?}", strays(&layout));
    }

    /// sb-go's `TestActivateGenerationAdoptsLegacyVenv`: the directory
    /// moves to `releases/legacy-<stamp>/venv`, its files with it.
    #[test]
    fn a_legacy_directory_is_adopted() {
        let (_dir, layout, venv) = staged();
        std::fs::create_dir_all(layout.active_venv.join("bin")).expect("legacy");
        std::fs::write(layout.active_venv.join("bin/ansible"), "legacy").expect("file");
        activate_generation(&layout, &venv).expect("activated");
        assert_eq!(std::fs::read_link(&layout.active_venv).expect("link"), venv);
        let names = releases(&layout);
        let legacy = names
            .iter()
            .find(|name| name.starts_with("legacy-"))
            .expect("legacy generation");
        let stamp = legacy.strip_prefix("legacy-").expect("stamp");
        assert_eq!(stamp.len(), "20260807T120000.000000000".len(), "{stamp}");
        let moved = layout
            .ansible_releases
            .join(legacy)
            .join("venv/bin/ansible");
        assert_eq!(std::fs::read_to_string(moved).expect("moved"), "legacy");
    }

    /// The switch fails: the legacy directory is back where it was, and no
    /// temporary link stays.
    #[test]
    fn a_failed_switch_puts_the_legacy_directory_back() {
        let (_dir, layout, venv) = staged();
        std::fs::create_dir_all(&layout.active_venv).expect("legacy");
        std::fs::write(layout.active_venv.join("marker"), "legacy").expect("file");
        let err = activate_with(&layout, &venv, |temporary, _| {
            drop(temporary);
            Err(io::Error::other("injected"))
        })
        .expect_err("injected");
        assert_eq!(err.to_string(), "activate Ansible venv: injected");
        let meta = std::fs::symlink_metadata(&layout.active_venv).expect("active");
        assert!(meta.is_dir(), "the legacy directory is back");
        assert!(layout.active_venv.join("marker").exists());
        assert!(strays(&layout).is_empty(), "{:?}", strays(&layout));
    }

    /// A failed rename through tempfile removes its temporary link.
    #[test]
    fn a_failed_rename_leaves_no_temporary_link() {
        let (_dir, layout, venv) = staged();
        // A non-empty directory can't be renamed over.
        std::fs::create_dir_all(layout.active_venv.join("x")).expect("dir");
        let _err = switch(&layout.active_venv, &venv, |temporary, link| {
            temporary.persist(link).map_err(|err| err.error)
        })
        .expect_err("over a directory");
        assert!(strays(&layout).is_empty(), "{:?}", strays(&layout));
    }

    /// A file at the active path is replaced as a link is (python-env
    /// spec, difference 17; sb-go refuses it).
    #[test]
    fn a_file_at_the_active_path_is_replaced() {
        let (_dir, layout, venv) = staged();
        std::fs::write(&layout.active_venv, "a file").expect("file");
        activate_generation(&layout, &venv).expect("replaced");
        assert_eq!(std::fs::read_link(&layout.active_venv).expect("link"), venv);
        assert!(strays(&layout).is_empty(), "{:?}", strays(&layout));
    }

    /// Any `lstat` failure but a missing path stops the switch.
    #[test]
    fn an_uninspectable_active_path_is_refused() {
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(found(Err(missing)).expect("missing"), Found::Replaceable);
        let err = found(Err(io::Error::other("injected"))).expect_err("other");
        assert_eq!(
            err.to_string(),
            "inspect active venv before switch: injected"
        );
    }

    #[test]
    fn activation_errors_are_sb_gos() {
        let (_dir, layout, venv) = staged();
        // `releases` is a file: it can't be made.
        std::fs::remove_dir_all(&layout.ansible_releases).expect("rm");
        std::fs::write(&layout.ansible_releases, "").expect("file");
        let err = activate_generation(&layout, &venv).expect_err("releases");
        assert!(
            err.to_string()
                .starts_with("create Ansible releases directory: "),
            "{err}"
        );
    }

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
