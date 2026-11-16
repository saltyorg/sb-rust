//! `copy_default_configs` (shared by setup and gha) and
//! `secure_existing_configs` (update's), ops spec "`copy_default_configs`"
//! and sb-go's saltbox/setup.go: each `<root>/defaults/*.default` names a
//! config file in the root, which can hold passwords and API keys. A
//! missing one is copied in, created exclusively with 0600; a present one
//! must be a regular file and gets 0600 through a handle that is checked
//! to be the same file; its content is never touched. The texts are
//! sb-go's.

use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use rustix::fs::{Mode, OFlags};
use sb_term_ui::{Labels, Step};

/// The defaults' suffix.
const SUFFIX: &[u8] = b".default";

/// The default files in `<root>/defaults`, in name order (Go's
/// `filepath.Glob` sorts), each with the config file it stands for in the
/// root. A missing directory has none, as the glob finds none.
///
/// # Errors
///
/// `error listing default config files: <cause>` when the directory
/// can't be read.
fn defaults(root: &Path) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    let dir = root.join("defaults");
    let listed = |err| anyhow!("error listing default config files: {err}");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(listed(err)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let name = entry.map_err(listed)?.file_name();
        // A name that is only the suffix stands for no file.
        let Some(stem) = name
            .as_bytes()
            .strip_suffix(SUFFIX)
            .filter(|stem| !stem.is_empty())
        else {
            continue;
        };
        found.push((dir.join(&name), root.join(OsStr::from_bytes(stem))));
    }
    found.sort();
    Ok(found)
}

/// Each missing config file copied from its default under `step` (a step
/// `Copying <name>.default` each), each present one secured (0600, as
/// [`secure_existing_configs`]).
///
/// # Errors
///
/// The listing's error, a destination that can't be inspected (`could
/// not check file <path>: <cause>`) or secured (as
/// [`secure_existing_configs`]), or a failed copy, shown in its step's
/// block (the error then carries the step's quiet `Exit`).
pub async fn copy_default_configs(step: &mut Step, root: &Path) -> anyhow::Result<()> {
    for (default, config) in defaults(root)? {
        match std::fs::symlink_metadata(&config) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                let name = default.file_name().unwrap_or_default().to_string_lossy();
                let label = format!("Copying {name}");
                step.step(Labels::new(&label, &label), async |_| {
                    copy(&default, &config)
                })
                .await?;
            }
            Err(err) => {
                return Err(anyhow!("could not check file {}: {err}", config.display()));
            }
            Ok(found) => secure(&config, &found)?,
        }
    }
    Ok(())
}

/// Each config file in `root` that has a default secured without copying
/// anything (sb-go's `SecureExistingConfigFiles`): a regular file gets
/// 0600; a missing one is skipped.
///
/// # Errors
///
/// The listing's error; `could not check file <path>: <cause>`; a
/// symlink (`configuration file <path> must not be a symlink`) or another
/// non-regular file (`configuration file <path> is not a regular file`);
/// a file that can't be opened, inspected or changed, or that changed
/// while it was opened (sb-go's words for each).
pub fn secure_existing_configs(root: &Path) -> anyhow::Result<()> {
    for (_, config) in defaults(root)? {
        match std::fs::symlink_metadata(&config) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(anyhow!("could not check file {}: {err}", config.display()));
            }
            Ok(found) => secure(&config, &found)?,
        }
    }
    Ok(())
}

/// `default` copied to a new `config`, created exclusively with 0600, so
/// an existing file or link is never followed and the copy is never
/// readable by others (`O_CREAT | O_EXCL`: std's `create_new`,
/// <https://doc.rust-lang.org/std/fs/struct.OpenOptions.html#method.create_new>),
/// then synced; a copy that fails is removed.
fn copy(default: &Path, config: &Path) -> anyhow::Result<()> {
    let mut source = File::open(default).map_err(failed("error opening source file", default))?;
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(config)
        .map_err(failed("error creating destination file", config))?;
    let written = io::copy(&mut source, &mut target)
        .map_err(|err| {
            let what = format!("error copying file {} to", default.display());
            failed(&what, config)(err)
        })
        .and_then(|_| {
            target
                .sync_all()
                .map_err(failed("error syncing destination file", config))
        });
    if written.is_err() {
        drop(target);
        let _ = std::fs::remove_file(config);
    }
    written
}

/// sb-go's error for a file operation: `<what> <path>: <cause>`.
fn failed(what: &str, path: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    let what = format!("{what} {}", path.display());
    move |err| anyhow!("{what}: {err}")
}

/// `config`, as `found` by `lstat`, given 0600: a regular file only,
/// through a handle opened without following a link (and without
/// blocking on a FIFO swapped in) and checked to be the file `lstat` saw
/// (`fchmod` on it: std's `File::set_permissions`,
/// <https://doc.rust-lang.org/std/fs/struct.File.html#method.set_permissions>;
/// rustix's `open`, <https://docs.rs/rustix/1.1.6/rustix/fs/fn.open.html>).
fn secure(config: &Path, found: &Metadata) -> anyhow::Result<()> {
    let shown = config.display();
    if found.file_type().is_symlink() {
        return Err(anyhow!("configuration file {shown} must not be a symlink"));
    }
    if !found.file_type().is_file() {
        return Err(anyhow!("configuration file {shown} is not a regular file"));
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = rustix::fs::open(config, flags, Mode::empty())
        .map(File::from)
        .map_err(|err| failed("error opening configuration file", config)(err.into()))?;
    let opened = file
        .metadata()
        .map_err(failed("error inspecting configuration file", config))?;
    if (opened.dev(), opened.ino()) != (found.dev(), found.ino()) {
        return Err(anyhow!(
            "configuration file {shown} changed while it was being opened"
        ));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(failed("error securing configuration file", config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link planted at the destination after the `lstat` (sb-go's
    /// `TestCopyConfigFileRejectsExistingSymlink`): the exclusive create
    /// refuses it, and its target is untouched.
    #[test]
    fn a_copy_never_follows_a_planted_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let default = dir.path().join("accounts.yml.default");
        std::fs::write(&default, "user: seed\n").expect("default");
        let target = dir.path().join("target");
        std::fs::write(&target, "kept\n").expect("target");
        let config = dir.path().join("accounts.yml");
        std::os::unix::fs::symlink(&target, &config).expect("link");
        let err = copy(&default, &config).expect_err("refused");
        assert!(
            err.to_string().starts_with(&format!(
                "error creating destination file {}: ",
                config.display()
            )),
            "{err}"
        );
        assert_eq!(std::fs::read_to_string(&target).expect("target"), "kept\n");
    }

    /// A default that can't be read leaves no copy behind; one that
    /// can't be opened names it.
    #[test]
    fn a_failed_copy_leaves_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("settings.yml");
        // A directory opens, then fails the read.
        let unreadable = dir.path().join("settings.yml.default");
        std::fs::create_dir(&unreadable).expect("dir");
        let err = copy(&unreadable, &config).expect_err("read fails");
        assert!(
            err.to_string().starts_with(&format!(
                "error copying file {} to {}: ",
                unreadable.display(),
                config.display()
            )),
            "{err}"
        );
        assert!(std::fs::symlink_metadata(&config).is_err());
        let missing = dir.path().join("gone.default");
        let err = copy(&missing, &config).expect_err("no source");
        assert!(
            err.to_string().starts_with(&format!(
                "error opening source file {}: ",
                missing.display()
            )),
            "{err}"
        );
        assert!(std::fs::symlink_metadata(&config).is_err());
    }
}
