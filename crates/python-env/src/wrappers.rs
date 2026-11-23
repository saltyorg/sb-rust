//! The venv's command wrappers (python-env spec, steps 12 and 14; sb-go's
//! `python/wrappers.go`): one small shell script per exported command in
//! `/usr/local/bin`, byte for byte sb-go's, so either binary recognises
//! and replaces the other's (decision 7).

use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use crate::Layout;

/// A wrapper's first two lines: what marks it as sb's (or sb-go's).
const HEADER: &str = "#!/bin/sh\n# Managed by sb-go Saltbox venv\n";

/// Why a wrapper couldn't be written or removed, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum WrapperError {
    /// A command name that isn't one plain file name.
    #[error("unsafe venv command name {0:?}")]
    UnsafeName(String),
    #[error("create wrapper for {}: {cause}", path.display())]
    Create { path: PathBuf, cause: io::Error },
    #[error("write wrapper for {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
    #[error("activate wrapper {}: {cause}", path.display())]
    Activate { path: PathBuf, cause: io::Error },
    #[error("remove stale wrapper {}: {cause}", path.display())]
    Remove { path: PathBuf, cause: io::Error },
    #[error("read wrapper {}: {cause}", path.display())]
    Inspect { path: PathBuf, cause: io::Error },
}

/// `name`'s wrapper path in `dir`, refusing a name that isn't one plain
/// file name (sb-go's `wrapperPath`: `filepath.Base(name) != name`, or a
/// line break).
fn wrapper_path(dir: &Path, name: &str) -> Result<PathBuf, WrapperError> {
    let plain = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains(['\n', '\r']);
    if plain {
        Ok(dir.join(name))
    } else {
        Err(WrapperError::UnsafeName(name.to_string()))
    }
}

/// sb-go's wrapper text for `name` (wrappers.go:75-86), with the layout's
/// active venv (`/srv/ansible/venv` in production).
fn content(layout: &Layout, name: &str) -> String {
    let active = layout.active_venv.display();
    format!(
        "{HEADER}venv_bin=$(/usr/bin/readlink -f {active}/bin) || exit 1\n\
         PATH=\"${{venv_bin}}${{PATH:+:${{PATH}}}}\"\nexport PATH\n\
         exec {active}/bin/{name} \"$@\"\n"
    )
}

/// Write the wrapper of each of `names` into the layout's wrapper
/// directory (sb-go's `installWrappers`): each through a temporary file
/// beside it (`.sb-wrapper-*`, tempfile's `Builder`,
/// <https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html>), mode
/// 0755 set on the open file (why not `Builder::permissions`: the umask
/// applies to it), then renamed over the old one
/// (`TempPath::persist`).
///
/// # Errors
///
/// [`WrapperError`]: the first name that's unsafe or can't be written.
pub fn write_all(layout: &Layout, names: &[String]) -> Result<(), WrapperError> {
    for name in names {
        let path = wrapper_path(&layout.wrappers, name)?;
        let text = content(layout, name);
        // Opened, written and closed (`into_temp_path` closes it,
        // <https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.into_temp_path>)
        // under cli-core's fork lock, so no child sb starts meanwhile
        // holds it open when sb runs ansible-playbook through it
        // (ETXTBSY, `fork_lock`).
        let file = sb_cli_core::fork_lock::writing(|| {
            let mut file = tempfile::Builder::new()
                .prefix(".sb-wrapper-")
                .tempfile_in(&layout.wrappers)
                .map_err(|cause| WrapperError::Create {
                    path: path.clone(),
                    cause,
                })?;
            let written = file.write_all(text.as_bytes()).and_then(|()| {
                file.as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o755))
            });
            written.map_err(|cause| WrapperError::Write {
                path: path.clone(),
                cause,
            })?;
            Ok::<_, WrapperError>(file.into_temp_path())
        })?;
        file.persist(&path).map_err(|err| WrapperError::Activate {
            path: path.clone(),
            cause: err.error,
        })?;
    }
    Ok(())
}

/// Whether the file at `path` starts with the exact two-line header
/// (sb-go's `isManagedWrapper`): a missing or shorter file isn't one.
fn managed(path: &Path) -> Result<bool, WrapperError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(cause) => {
            return Err(WrapperError::Inspect {
                path: path.to_path_buf(),
                cause,
            });
        }
    };
    let mut head = Vec::with_capacity(HEADER.len());
    file.take(HEADER.len() as u64)
        .read_to_end(&mut head)
        .map_err(|cause| WrapperError::Inspect {
            path: path.to_path_buf(),
            cause,
        })?;
    Ok(head == HEADER.as_bytes())
}

/// Remove the wrappers `previous` listed and `current` doesn't, each only
/// when it carries the header (sb-go's `removeStaleWrappers`): a user's own
/// script of the same name stays.
///
/// # Errors
///
/// [`WrapperError`]: an unsafe name, or a wrapper that can't be read or
/// removed.
pub fn remove_stale(
    layout: &Layout,
    previous: &[String],
    current: &[String],
) -> Result<(), WrapperError> {
    for name in previous.iter().filter(|name| !current.contains(name)) {
        let path = wrapper_path(&layout.wrappers, name)?;
        if managed(&path)? {
            std::fs::remove_file(&path).map_err(|cause| WrapperError::Remove { path, cause })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sb-go's wrapper for `ansible-lint`, byte for byte
    /// (wrappers.go:75-86 with layout.AnsibleVenvPath).
    const SB_GO_ANSIBLE_LINT: &str = "#!/bin/sh\n\
        # Managed by sb-go Saltbox venv\n\
        venv_bin=$(/usr/bin/readlink -f /srv/ansible/venv/bin) || exit 1\n\
        PATH=\"${venv_bin}${PATH:+:${PATH}}\"\n\
        export PATH\n\
        exec /srv/ansible/venv/bin/ansible-lint \"$@\"\n";

    #[test]
    fn the_wrapper_is_sb_gos_byte_for_byte() {
        assert_eq!(
            content(&Layout::saltbox(), "ansible-lint"),
            SB_GO_ANSIBLE_LINT
        );
    }

    fn tree() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = Layout::under(dir.path());
        std::fs::create_dir_all(&layout.wrappers).expect("bin");
        (dir, layout)
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    /// Each wrapper is written under cli-core's fork lock: never while a
    /// child starts, which would inherit its write handle (ETXTBSY when
    /// sb runs ansible-playbook through it).
    #[test]
    fn a_wrapper_is_not_written_while_a_child_starts() {
        let (_dir, layout) = tree();
        let layout = sb_cli_core::testing::waits_for_a_fork(move || {
            write_all(&layout, &names(&["ansible"])).map(|()| layout)
        })
        .expect("written");
        let text = std::fs::read_to_string(layout.wrappers.join("ansible")).expect("wrapper");
        assert_eq!(text, content(&layout, "ansible"));
    }

    #[test]
    fn wrappers_are_written_0755_over_the_old_ones() {
        let (_dir, layout) = tree();
        let path = layout.wrappers.join("ansible");
        std::fs::write(&path, "old").expect("old");
        write_all(&layout, &names(&["ansible", "certbot"])).expect("written");
        for name in ["ansible", "certbot"] {
            let path = layout.wrappers.join(name);
            let text = std::fs::read_to_string(&path).expect("wrapper");
            assert_eq!(text, content(&layout, name));
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o7777, 0o755);
        }
        // No temporary file left beside them.
        let left: Vec<_> = std::fs::read_dir(&layout.wrappers)
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
    }

    #[test]
    fn an_unsafe_name_is_refused() {
        let (_dir, layout) = tree();
        for bad in [
            "../ansible",
            "a/b",
            "ansible\nx",
            "ansible\r",
            "",
            ".",
            "..",
        ] {
            let err = write_all(&layout, &names(&[bad])).expect_err(bad);
            assert_eq!(err.to_string(), format!("unsafe venv command name {bad:?}"));
        }
    }

    /// sb-go's `TestStaleWrapperCleanupOnlyUsesPreviousManifest` and
    /// `TestManagedWrapperDetectionRequiresExactHeader`: only a wrapper the
    /// previous manifest listed, the new one doesn't, and that starts with
    /// the exact header goes.
    #[test]
    fn only_managed_stale_wrappers_are_removed() {
        let (_dir, layout) = tree();
        write_all(&layout, &names(&["current", "stale"])).expect("written");
        let user_script = layout.wrappers.join("ansible-lint");
        std::fs::write(&user_script, "#!/bin/sh\necho mine\n").expect("user script");
        let binary = layout.wrappers.join("binary");
        let mut bytes = b"\x7fELF compiled program data: ".to_vec();
        bytes.extend_from_slice(HEADER.as_bytes());
        std::fs::write(&binary, bytes).expect("binary");
        let short = layout.wrappers.join("short");
        std::fs::write(&short, "#!/bin/sh\n").expect("short");
        let previous = names(&[
            "current",
            "stale",
            "ansible-lint",
            "binary",
            "short",
            "absent",
        ]);
        remove_stale(&layout, &previous, &names(&["current"])).expect("removed");
        assert!(layout.wrappers.join("current").exists());
        assert!(!layout.wrappers.join("stale").exists());
        assert!(user_script.exists(), "a user's own script stays");
        assert!(binary.exists());
        assert!(short.exists());
    }

    #[test]
    fn errors_name_the_wrapper() {
        let (dir, layout) = tree();
        std::fs::remove_dir(&layout.wrappers).expect("rm bin");
        let err = write_all(&layout, &names(&["ansible"])).expect_err("no dir");
        let path = layout.wrappers.join("ansible");
        assert!(
            err.to_string()
                .starts_with(&format!("create wrapper for {}: ", path.display())),
            "{err}"
        );
        drop(dir);
    }

    /// A stale wrapper that can't be opened (a link to itself) is its read
    /// error, naming it, and stays.
    #[test]
    fn a_stale_wrapper_that_cant_be_opened_is_its_error() {
        let (_dir, layout) = tree();
        let path = layout.wrappers.join("ansible");
        std::os::unix::fs::symlink(&path, &path).expect("a loop");
        let err = remove_stale(&layout, &names(&["ansible"]), &[]).expect_err("a loop");
        assert!(
            matches!(&err, WrapperError::Inspect { path: named, .. } if *named == path),
            "{err:?}"
        );
        assert!(
            err.to_string()
                .starts_with(&format!("read wrapper {}: ", path.display())),
            "{err}"
        );
        assert!(path.symlink_metadata().is_ok(), "it stays");
    }

    /// A directory where a wrapper goes is never replaced, and isn't read
    /// as a stale wrapper: each is the wrapper's error, naming it; the
    /// directory stays, and no temporary file is left beside it.
    #[test]
    fn a_directory_at_a_wrappers_path_is_its_error() {
        let (dir, layout) = tree();
        let path = layout.wrappers.join("ansible");
        std::fs::create_dir(&path).expect("dir");
        std::fs::write(path.join("inside"), "x").expect("inside");
        let err = write_all(&layout, &names(&["ansible"])).expect_err("a directory");
        assert!(
            err.to_string()
                .starts_with(&format!("activate wrapper {}: ", path.display())),
            "{err}"
        );
        let err = remove_stale(&layout, &names(&["ansible"]), &[]).expect_err("a directory");
        assert!(
            err.to_string()
                .starts_with(&format!("read wrapper {}: ", path.display())),
            "{err}"
        );
        assert!(path.join("inside").exists());
        // The new wrapper's temporary file went with the failure.
        let left: Vec<String> = std::fs::read_dir(&layout.wrappers)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with(".sb-wrapper-"))
            .collect();
        assert_eq!(left, Vec::<String>::new());
        drop(dir);
    }
}
