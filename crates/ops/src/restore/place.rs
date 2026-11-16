//! Where a restored file goes (ops spec, "Placement"; decision 9): every
//! file at the Saltbox root's top level, root-owned, mode 0600, an
//! existing one overwritten without a backup, as sb-go leaves them
//! (cmd/restore.go, `moveFile(..., 0600)`). sb's means: each plaintext
//! goes to a temporary file in the root and is renamed over its name, so
//! a link at the name is replaced, never followed, and a reader sees the
//! old file or the new one.

use std::fs::{self, DirBuilder, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::Path;

/// The files the restore service keeps, in sb-go's order (cmd/restore.go,
/// `validateAndRestore`).
pub const FILES: [&str; 9] = [
    "accounts.yml",
    "adv_settings.yml",
    "backup_config.yml",
    "hetzner_vlan.yml",
    "localhost.yml",
    "motd.yml",
    "providers.yml",
    "rclone.conf",
    "settings.yml",
];

/// The Saltbox root's mode when restore creates it (sb-go's
/// `os.MkdirAll(dir, 0755)`).
const ROOT_MODE: u32 = 0o755;

/// Write `plaintext` as `<root>/<file>`, `file` one of [`FILES`].
///
/// - `root` is created, with its missing parents, when it doesn't exist,
///   and set to 0755 (restore normally runs after setup made it).
/// - The plaintext goes to a `tempfile::NamedTempFile` in `root` (set to
///   0600, created exclusively under an unpredictable name), is synced,
///   and is persisted over `<root>/<file>` with `rename`
///   (<https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist>):
///   a link at the name is replaced, its target untouched; an existing
///   file is overwritten. The file is root's as sb runs as root; no chown.
///   The root is synced after the rename, so the new name survives a
///   power cut.
/// - On any failure the temporary file is removed (`NamedTempFile`'s
///   drop, and `PersistError`'s file dropped with it).
///
/// # Errors
///
/// `InvalidInput` for a name not in [`FILES`] (nothing is written), or
/// the I/O error of creating the root, writing, syncing or renaming.
/// A signal's forced exit (cli-core's second signal) or SIGKILL during the
/// write can leave a `.sb-restore-*` file (0600, root's) in the root.
pub fn place(root: &Path, file: &str, plaintext: &[u8]) -> io::Result<()> {
    if !FILES.contains(&file) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a restore file: {file}"),
        ));
    }
    if !root.exists() {
        DirBuilder::new()
            .recursive(true)
            .mode(ROOT_MODE)
            .create(root)?;
        // The umask may have narrowed it.
        fs::set_permissions(root, Permissions::from_mode(ROOT_MODE))?;
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".sb-restore-")
        .tempfile_in(root)?;
    // 0600 whatever the umask, as sb-go's `moveFile(..., 0600)` sets it.
    temporary
        .as_file()
        .set_permissions(Permissions::from_mode(FILE_MODE))?;
    temporary.write_all(plaintext)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(root.join(file))
        .map_err(|failed| failed.error)?;
    // The rename reaches the disk with the directory: tempfile leaves
    // that to the caller ("neither the file contents nor the containing
    // directory are synchronized", `NamedTempFile::persist`), as
    // sb-host's apt sources do.
    fs::File::open(root)?.sync_all()
}

/// A restored file's mode (sb-go's).
const FILE_MODE: u32 = 0o600;

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, symlink};

    use super::*;

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).expect("metadata").mode() & 0o7777
    }

    /// The names left in `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("list")
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
    fn a_file_lands_at_the_root_with_mode_0600() {
        let root = tempfile::tempdir().expect("root");
        place(root.path(), "settings.yml", b"a: 1\n").expect("placed");
        let path = root.path().join("settings.yml");
        assert_eq!(fs::read(&path).expect("read"), b"a: 1\n");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(
            names(root.path()),
            ["settings.yml"],
            "no temporary file left"
        );
    }

    #[test]
    fn a_missing_root_is_created_0755() {
        let parent = tempfile::tempdir().expect("parent");
        let root = parent.path().join("git/saltbox");
        place(&root, "accounts.yml", b"x").expect("placed");
        assert_eq!(mode(&root), 0o755);
        assert_eq!(fs::read(root.join("accounts.yml")).expect("read"), b"x");
    }

    /// An existing root keeps its mode: restore only sets it when it
    /// creates it.
    #[test]
    fn an_existing_root_keeps_its_mode() {
        let root = tempfile::tempdir().expect("root");
        fs::set_permissions(root.path(), Permissions::from_mode(0o750)).expect("chmod");
        place(root.path(), "motd.yml", b"x").expect("placed");
        assert_eq!(mode(root.path()), 0o750);
    }

    #[test]
    fn an_existing_file_is_overwritten() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("rclone.conf");
        fs::write(&path, "old").expect("old");
        fs::set_permissions(&path, Permissions::from_mode(0o644)).expect("chmod");
        place(root.path(), "rclone.conf", b"new").expect("placed");
        assert_eq!(fs::read(&path).expect("read"), b"new");
        assert_eq!(mode(&path), 0o600);
    }

    /// A link at the name is replaced by the file; what it pointed to is
    /// left as it was.
    #[test]
    fn a_link_at_the_name_is_replaced_not_followed() {
        let root = tempfile::tempdir().expect("root");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let target = elsewhere.path().join("target");
        fs::write(&target, "untouched").expect("target");
        let path = root.path().join("localhost.yml");
        symlink(&target, &path).expect("link");
        place(root.path(), "localhost.yml", b"restored").expect("placed");
        assert!(
            !fs::symlink_metadata(&path)
                .expect("meta")
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&path).expect("read"), b"restored");
        assert_eq!(fs::read(&target).expect("target"), b"untouched");
    }

    /// A rename that fails (a directory at the name) leaves no temporary
    /// file behind, and the directory as it was.
    #[test]
    fn a_failed_placement_leaves_no_temporary_file() {
        let root = tempfile::tempdir().expect("root");
        fs::create_dir(root.path().join("providers.yml")).expect("dir");
        assert!(place(root.path(), "providers.yml", b"x").is_err());
        assert_eq!(names(root.path()), ["providers.yml"]);
        assert!(root.path().join("providers.yml").is_dir());
    }

    /// Only the service's files: a name outside the list, a path among
    /// them, writes nothing.
    #[test]
    fn only_the_restore_files_are_placed() {
        let root = tempfile::tempdir().expect("root");
        for name in ["../escape.yml", "sub/settings.yml", "other.yml", ""] {
            let err = place(root.path(), name, b"x").expect_err(name);
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{name}");
        }
        assert_eq!(names(root.path()), Vec::<String>::new());
    }

    /// sb-go's list and order.
    #[test]
    fn the_files_are_sb_gos_nine() {
        assert_eq!(
            FILES.join(" "),
            "accounts.yml adv_settings.yml backup_config.yml hetzner_vlan.yml localhost.yml \
             motd.yml providers.yml rclone.conf settings.yml"
        );
    }
}
