//! File ownership (host spec, "Ownership"; sb-go's host/ownership.go):
//! give paths, or whole trees, to the Saltbox user and its primary group.
//!
//! - [`ensure_owner`] changes each path itself;
//! - [`ensure_owner_recursive`] changes each root and everything under it.
//!
//! Both skip what isn't there: an empty or unknown user, a missing path or
//! root. A symlink is changed itself, never its target. The run log gets
//! one count of changed entries per root, never each path: a Saltbox
//! checkout holds thousands.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nix::unistd::User;
use walkdir::WalkDir;

/// A user's uid and primary gid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ids {
    uid: u32,
    gid: u32,
}

/// Why ownership couldn't be set, in sb-go's words.
///
/// The cause is part of the text only, not a `source` as well: the failure
/// block shows the whole chain, which would print it twice (as
/// [`crate::journal::UserLookupError`]).
#[derive(Debug, thiserror::Error)]
pub enum OwnerError {
    /// The user database couldn't be read.
    #[error("look up user {user}: {cause}")]
    Lookup { user: String, cause: io::Error },
    /// A path couldn't be inspected (`lstat`, or reading a directory in a
    /// walk).
    #[error("inspect ownership of {}: {cause}", path.display())]
    Inspect { path: PathBuf, cause: io::Error },
    /// `lchown` failed.
    #[error("set ownership of {}: {cause}", path.display())]
    Set { path: PathBuf, cause: io::Error },
    /// A recursive change failed on an entry under `root`.
    #[error("set recursive ownership of {}: {cause}", root.display())]
    Recursive {
        root: PathBuf,
        cause: Box<OwnerError>,
    },
}

/// Give each of `paths` to `user` and its primary group, without
/// descending into directories (sb-go's `EnsureForExistingUser`).
///
/// A path is changed only when its uid or gid differs. A missing path is
/// skipped, and an empty or unknown user skips everything. A symlink is
/// changed itself (`lchown`), never its target.
///
/// The user is looked up with nix's `User::from_name`, glibc's
/// `getpwnam_r` through NSS
/// (<https://docs.rs/nix/0.28.0/nix/unistd/struct.User.html#method.from_name>;
/// host spec, decision 2). It blocks, as an NSS lookup can: on a runtime,
/// run it under `spawn_blocking`.
///
/// # Errors
///
/// [`OwnerError::Lookup`] when the user database can't be read,
/// [`OwnerError::Inspect`] when a path can't be inspected for a reason
/// other than its absence, [`OwnerError::Set`] when `lchown` fails. The
/// first error ends the call.
pub fn ensure_owner<P: AsRef<Path>>(user: &str, paths: &[P]) -> Result<(), OwnerError> {
    let Some(ids) = ids_with(user, lookup)? else {
        skipped(user);
        return Ok(());
    };
    for path in paths {
        let path = path.as_ref();
        let changed = ensure_path(path, ids, &mut lchown)?;
        tracing::info!(path = %path.display(), changed, "ownership");
    }
    Ok(())
}

/// Give each of `roots`, and everything under it, to `user` and its
/// primary group (sb-go's `EnsureRecursiveForExistingUser`).
///
/// The walk is walkdir's, following no symlink, the root's included
/// (`follow_links(false)`, `follow_root_links(false)`:
/// <https://docs.rs/walkdir/2.5.0/walkdir/struct.WalkDir.html#method.follow_root_links>):
/// a symlinked root changes only the link, as sb-go's `filepath.WalkDir`
/// does. A missing root is skipped, and an empty or unknown user skips
/// everything. An entry that vanishes during the walk is skipped and the
/// walk goes on; sb-go ended the whole walk there (host spec, difference
/// 11).
///
/// # Errors
///
/// [`OwnerError::Lookup`] when the user database can't be read;
/// [`OwnerError::Recursive`] around an entry that can't be inspected or
/// changed. The first error ends the call.
pub fn ensure_owner_recursive<P: AsRef<Path>>(user: &str, roots: &[P]) -> Result<(), OwnerError> {
    let Some(ids) = ids_with(user, lookup)? else {
        skipped(user);
        return Ok(());
    };
    for root in roots {
        let root = root.as_ref();
        let changed = ensure_tree(root, ids, &mut lchown)?;
        tracing::info!(root = %root.display(), changed, "recursive ownership");
    }
    Ok(())
}

/// The run-log record for a call skipped because `user` is empty or
/// unknown: early in setup, before the user exists, that tells "nothing to
/// own yet" from "never asked".
fn skipped(user: &str) {
    tracing::info!(user, "ownership skipped: no such user");
}

/// The system's user lookup: `user`'s uid and primary gid, or `None` when
/// there's no such user.
fn lookup(user: &str) -> nix::Result<Option<Ids>> {
    User::from_name(user).map(|found| {
        found.map(|account| Ids {
            uid: account.uid.as_raw(),
            gid: account.gid.as_raw(),
        })
    })
}

/// std's `lchown`
/// (<https://doc.rust-lang.org/std/os/unix/fs/fn.lchown.html>), with both
/// ids.
fn lchown(path: &Path, ids: Ids) -> io::Result<()> {
    std::os::unix::fs::lchown(path, Some(ids.uid), Some(ids.gid))
}

/// `user`'s ids as `lookup` finds them: `None` for an empty or unknown
/// user (M-MOCKABLE-SYSCALLS: the tests inject the lookup).
///
/// glibc documents that some NSS backends report a missing user as an
/// error (`ENOENT`, `ESRCH`, `EBADF`, `EPERM`) rather than none; with the
/// files and systemd backends a Saltbox host uses, a missing user is
/// `Ok(None)`. UNVERIFIED for other backends, which fail here as sb-go's
/// lookup would on an unreadable database.
fn ids_with(
    user: &str,
    lookup: impl FnOnce(&str) -> nix::Result<Option<Ids>>,
) -> Result<Option<Ids>, OwnerError> {
    if user.is_empty() {
        return Ok(None);
    }
    lookup(user).map_err(|errno| OwnerError::Lookup {
        user: user.to_string(),
        cause: io::Error::from(errno),
    })
}

/// Change `path` itself when its ids differ from `ids`: whether it was
/// changed. A missing path is skipped.
fn ensure_path(
    path: &Path,
    ids: Ids,
    lchown: &mut impl FnMut(&Path, Ids) -> io::Result<()>,
) -> Result<bool, OwnerError> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(cause) => {
            return Err(OwnerError::Inspect {
                path: path.to_path_buf(),
                cause,
            });
        }
    };
    if metadata.uid() == ids.uid && metadata.gid() == ids.gid {
        return Ok(false);
    }
    lchown(path, ids).map_err(|cause| OwnerError::Set {
        path: path.to_path_buf(),
        cause,
    })?;
    Ok(true)
}

/// Change `root` and everything under it whose ids differ from `ids`: the
/// count of entries changed. A missing root, and any entry that vanishes
/// mid-walk, is skipped.
fn ensure_tree(
    root: &Path,
    ids: Ids,
    lchown: &mut impl FnMut(&Path, Ids) -> io::Result<()>,
) -> Result<usize, OwnerError> {
    let wrap = |cause| OwnerError::Recursive {
        root: root.to_path_buf(),
        cause: Box::new(cause),
    };
    let mut changed = 0;
    let walk = WalkDir::new(root)
        .follow_links(false)
        .follow_root_links(false);
    for entry in walk {
        // walkdir's metadata is `symlink_metadata` when links aren't
        // followed (<https://docs.rs/walkdir/2.5.0/walkdir/struct.DirEntry.html#method.metadata>).
        let metadata = entry.and_then(|entry| entry.metadata().map(|meta| (entry, meta)));
        let (entry, metadata) = match metadata {
            Ok(found) => found,
            Err(error) => match inspect_failure(error) {
                None => continue,
                Some(failure) => return Err(wrap(failure)),
            },
        };
        if metadata.uid() == ids.uid && metadata.gid() == ids.gid {
            continue;
        }
        match lchown(entry.path(), ids) {
            Ok(()) => changed += 1,
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
            Err(cause) => {
                return Err(wrap(OwnerError::Set {
                    path: entry.path().to_path_buf(),
                    cause,
                }));
            }
        }
    }
    Ok(changed)
}

/// A walk error as sb-go's text, or `None` for an entry that isn't there
/// (any more), which the walk skips.
fn inspect_failure(error: walkdir::Error) -> Option<OwnerError> {
    let path = error.path().map_or_default(Path::to_path_buf);
    // Without followed links walkdir reports no loop, so every error
    // carries an I/O error; `other` keeps the text if one ever doesn't.
    let cause = match error.into_io_error() {
        Some(cause) if cause.kind() == io::ErrorKind::NotFound => return None,
        Some(cause) => cause,
        None => io::Error::other("walk failed"),
    };
    Some(OwnerError::Inspect { path, cause })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    /// Ids no file in a fresh temp tree has.
    const OTHER: Ids = Ids {
        uid: 1 << 30,
        gid: 1 << 30,
    };

    /// The ids `path` has now.
    fn ids_of(path: &Path) -> Ids {
        let metadata = path.symlink_metadata().expect("lstat");
        Ids {
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
    }

    /// An `lchown` that records each path and changes nothing.
    fn recorder(seen: &RefCell<Vec<PathBuf>>) -> impl FnMut(&Path, Ids) -> io::Result<()> + '_ {
        |path, _| {
            seen.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
    }

    #[test]
    fn an_empty_user_skips_without_a_lookup() {
        let found = ids_with("", |_| panic!("looked up")).expect("skipped");
        assert_eq!(found, None);
    }

    #[test]
    fn an_unknown_user_skips() {
        assert_eq!(
            ids_with("nobody-here", |_| Ok(None)).expect("skipped"),
            None
        );
    }

    #[test]
    fn a_known_user_gives_its_ids() {
        let ids = Ids { uid: 7, gid: 8 };
        assert_eq!(
            ids_with("saltbox", |_| Ok(Some(ids))).expect("ids"),
            Some(ids)
        );
    }

    #[test]
    fn a_failed_lookup_names_the_user() {
        let error = ids_with("seed", |_| Err(nix::errno::Errno::EIO)).expect_err("failed");
        assert_eq!(
            error.to_string(),
            "look up user seed: Input/output error (os error 5)"
        );
    }

    #[test]
    fn the_system_lookup_finds_root_and_misses_an_unknown_user() {
        assert_eq!(
            lookup("root").expect("lookup"),
            Some(Ids { uid: 0, gid: 0 })
        );
        assert_eq!(
            lookup("sb-rust-user-that-does-not-exist-7fcd24d1").expect("lookup"),
            None
        );
    }

    #[test]
    fn the_public_calls_skip_an_unknown_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let unknown = "sb-rust-user-that-does-not-exist-7fcd24d1";
        ensure_owner(unknown, &[dir.path()]).expect("skipped");
        ensure_owner_recursive(unknown, &[dir.path()]).expect("skipped");
        ensure_owner("", &[dir.path()]).expect("skipped");
        ensure_owner_recursive("", &[dir.path()]).expect("skipped");
    }

    #[test]
    fn the_public_calls_keep_the_current_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("file"), "x").expect("file");
        let me = User::from_uid(nix::unistd::geteuid())
            .expect("lookup")
            .expect("current user");
        let before = ids_of(&dir.path().join("file"));
        ensure_owner(&me.name, &[dir.path()]).expect("owner");
        ensure_owner_recursive(&me.name, &[dir.path()]).expect("recursive");
        assert_eq!(ids_of(&dir.path().join("file")), before);
    }

    #[test]
    fn the_system_lchown_reports_a_missing_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = lchown(&dir.path().join("missing"), OTHER).expect_err("missing");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_path_with_other_ids_is_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = RefCell::new(Vec::new());
        let changed = ensure_path(dir.path(), OTHER, &mut recorder(&seen)).expect("owner");
        assert!(changed);
        assert_eq!(*seen.borrow(), [dir.path().to_path_buf()]);
    }

    #[test]
    fn a_path_with_the_same_ids_is_left() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = RefCell::new(Vec::new());
        let ids = ids_of(dir.path());
        let changed = ensure_path(dir.path(), ids, &mut recorder(&seen)).expect("owner");
        assert!(!changed);
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn a_path_differing_only_in_uid_or_gid_is_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ids = ids_of(dir.path());
        for differing in [
            Ids {
                uid: OTHER.uid,
                ..ids
            },
            Ids {
                gid: OTHER.gid,
                ..ids
            },
        ] {
            let seen = RefCell::new(Vec::new());
            assert!(ensure_path(dir.path(), differing, &mut recorder(&seen)).expect("owner"));
            assert_eq!(seen.borrow().len(), 1);
        }
    }

    #[test]
    fn a_missing_path_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = RefCell::new(Vec::new());
        let missing = dir.path().join("missing");
        assert!(!ensure_path(&missing, OTHER, &mut recorder(&seen)).expect("skipped"));
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn a_symlink_is_changed_itself_not_its_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        fs::write(&target, "x").expect("target");
        let link = dir.path().join("link");
        symlink(&target, &link).expect("link");
        let seen = RefCell::new(Vec::new());
        ensure_path(&link, OTHER, &mut recorder(&seen)).expect("owner");
        assert_eq!(*seen.borrow(), [link]);
    }

    #[test]
    fn a_dangling_symlink_is_changed_not_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let link = dir.path().join("link");
        symlink(dir.path().join("gone"), &link).expect("link");
        let seen = RefCell::new(Vec::new());
        assert!(ensure_path(&link, OTHER, &mut recorder(&seen)).expect("owner"));
    }

    #[test]
    fn an_uninspectable_path_names_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("file");
        fs::write(&file, "x").expect("file");
        let below = file.join("below");
        let seen = RefCell::new(Vec::new());
        let error = ensure_path(&below, OTHER, &mut recorder(&seen)).expect_err("failed");
        assert_eq!(
            error.to_string(),
            format!(
                "inspect ownership of {}: Not a directory (os error 20)",
                below.display()
            )
        );
    }

    #[test]
    fn a_failed_lchown_names_the_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = ensure_path(dir.path(), OTHER, &mut |_, _| {
            Err(io::Error::from_raw_os_error(1))
        })
        .expect_err("failed");
        assert_eq!(
            error.to_string(),
            format!(
                "set ownership of {}: Operation not permitted (os error 1)",
                dir.path().display()
            )
        );
    }

    #[test]
    fn a_tree_is_changed_entry_by_entry_and_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir(root.join("sub")).expect("sub");
        fs::write(root.join("sub/file"), "x").expect("file");
        fs::write(root.join("top"), "x").expect("top");
        let seen = RefCell::new(Vec::new());
        let changed = ensure_tree(root, OTHER, &mut recorder(&seen)).expect("tree");
        assert_eq!(changed, 4);
        let mut seen = seen.into_inner();
        seen.sort();
        assert_eq!(
            seen,
            [
                root.to_path_buf(),
                root.join("sub"),
                root.join("sub/file"),
                root.join("top")
            ]
        );
    }

    #[test]
    fn a_tree_with_the_same_ids_changes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("file"), "x").expect("file");
        let seen = RefCell::new(Vec::new());
        let ids = ids_of(dir.path());
        let changed = ensure_tree(dir.path(), ids, &mut recorder(&seen)).expect("tree");
        assert_eq!(changed, 0);
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn a_tree_entry_differing_only_in_uid_or_gid_is_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ids = ids_of(dir.path());
        for differing in [
            Ids {
                uid: OTHER.uid,
                ..ids
            },
            Ids {
                gid: OTHER.gid,
                ..ids
            },
        ] {
            let seen = RefCell::new(Vec::new());
            assert_eq!(
                ensure_tree(dir.path(), differing, &mut recorder(&seen)).expect("tree"),
                1
            );
        }
    }

    #[test]
    fn a_tree_changes_a_symlink_not_its_target() {
        let root = tempfile::tempdir().expect("root");
        let outside = tempfile::tempdir().expect("outside");
        let target = outside.path().join("target");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("inner"), "x").expect("inner");
        let link = root.path().join("active");
        symlink(&target, &link).expect("link");
        let seen = RefCell::new(Vec::new());
        ensure_tree(root.path(), OTHER, &mut recorder(&seen)).expect("tree");
        let seen = seen.into_inner();
        assert!(seen.contains(&root.path().to_path_buf()), "{seen:?}");
        assert!(seen.contains(&link), "{seen:?}");
        assert!(
            !seen.iter().any(|path| path.starts_with(outside.path())),
            "{seen:?}"
        );
    }

    #[test]
    fn a_symlinked_root_changes_only_the_link() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("inner"), "x").expect("inner");
        let link = dir.path().join("link");
        symlink(&target, &link).expect("link");
        let seen = RefCell::new(Vec::new());
        assert_eq!(
            ensure_tree(&link, OTHER, &mut recorder(&seen)).expect("tree"),
            1
        );
        assert_eq!(*seen.borrow(), [link]);
    }

    #[test]
    fn a_missing_root_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = RefCell::new(Vec::new());
        let missing = dir.path().join("missing");
        assert_eq!(
            ensure_tree(&missing, OTHER, &mut recorder(&seen)).expect("skipped"),
            0
        );
        assert!(seen.borrow().is_empty());
    }

    #[test]
    fn a_directory_that_vanishes_mid_walk_is_skipped_and_the_walk_goes_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for sub in ["a", "b"] {
            fs::create_dir(root.join(sub)).expect("sub");
            fs::write(root.join(sub).join("file"), "x").expect("file");
        }
        let gone = root.join("a");
        let mut seen = Vec::new();
        // Removing `a` when it's changed, before walkdir opens it: its
        // listing fails with ENOENT.
        let changed = ensure_tree(root, OTHER, &mut |path: &Path, _| {
            if path == gone {
                fs::remove_dir_all(path).expect("remove");
            }
            seen.push(path.to_path_buf());
            Ok(())
        })
        .expect("tree");
        seen.sort();
        assert_eq!(
            seen,
            [
                root.to_path_buf(),
                root.join("a"),
                root.join("b"),
                root.join("b/file")
            ]
        );
        assert_eq!(changed, 4);
    }

    #[test]
    fn an_entry_that_vanishes_before_lchown_is_skipped_uncounted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("gone"), "x").expect("gone");
        fs::write(root.join("kept"), "x").expect("kept");
        let mut seen = Vec::new();
        let changed = ensure_tree(root, OTHER, &mut |path: &Path, _| {
            if path.ends_with("gone") {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            seen.push(path.to_path_buf());
            Ok(())
        })
        .expect("tree");
        assert_eq!(changed, 2);
        seen.sort();
        assert_eq!(seen, [root.to_path_buf(), root.join("kept")]);
    }

    #[test]
    fn a_file_that_vanishes_before_its_lstat_is_skipped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        for name in ["one", "two"] {
            fs::write(root.join(name), "x").expect("file");
        }
        // The directory is listed in one read (two names), so whichever
        // file comes first removes the other after the listing, and the
        // other's lstat finds nothing. UNVERIFIED that every filesystem
        // lists both names in one getdents call; tmpfs and ext4 do.
        let mut seen = Vec::new();
        let changed = ensure_tree(root, OTHER, &mut |path: &Path, _| {
            if path != root {
                for name in ["one", "two"] {
                    let other = root.join(name);
                    if other != path {
                        fs::remove_file(other).ok();
                    }
                }
            }
            seen.push(path.to_path_buf());
            Ok(())
        })
        .expect("tree");
        assert_eq!(changed, 2, "{seen:?}");
    }

    #[test]
    fn an_uninspectable_root_nests_the_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("file");
        fs::write(&file, "x").expect("file");
        let below = file.join("below");
        let seen = RefCell::new(Vec::new());
        let error = ensure_tree(&below, OTHER, &mut recorder(&seen)).expect_err("failed");
        assert_eq!(
            error.to_string(),
            format!(
                "set recursive ownership of {0}: inspect ownership of {0}: Not a directory (os error 20)",
                below.display()
            )
        );
    }

    #[test]
    fn a_failed_lchown_in_a_tree_nests_the_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::write(root.join("file"), "x").expect("file");
        let file = root.join("file");
        let error = ensure_tree(root, OTHER, &mut |path: &Path, _| {
            if path == file {
                Err(io::Error::from_raw_os_error(1))
            } else {
                Ok(())
            }
        })
        .expect_err("failed");
        assert_eq!(
            error.to_string(),
            format!(
                "set recursive ownership of {}: set ownership of {}: Operation not permitted (os error 1)",
                root.display(),
                file.display()
            )
        );
    }

    #[test]
    fn a_walk_error_without_an_io_error_keeps_a_text() {
        // Only a followed link can loop, and links aren't followed; the
        // fallback is still checked through a looping walk.
        let dir = tempfile::tempdir().expect("tempdir");
        symlink(dir.path(), dir.path().join("loop")).expect("loop");
        let error = WalkDir::new(dir.path())
            .follow_links(true)
            .into_iter()
            .find_map(Result::err)
            .expect("loop error");
        let failure = inspect_failure(error).expect("failure");
        assert_eq!(
            failure.to_string(),
            format!(
                "inspect ownership of {}: walk failed",
                dir.path().join("loop").display()
            )
        );
    }
}
