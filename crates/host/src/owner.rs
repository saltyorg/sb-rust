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

use std::fs::{FileType, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use nix::unistd::User;

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
/// (<https://docs.rs/nix/0.31.3/nix/unistd/struct.User.html#method.from_name>;
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
/// The walk follows no symlink, the root's included, as walkdir's with
/// `follow_links(false)` and `follow_root_links(false)` did (`Walk`):
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
    for found in Walk::new(root, MAX_OPEN) {
        let found = match found {
            Ok(found) => found,
            Err(OwnerError::Inspect { cause, .. }) if cause.kind() == io::ErrorKind::NotFound => {
                continue;
            }
            Err(failure) => return Err(wrap(failure)),
        };
        if found.metadata.uid() == ids.uid && found.metadata.gid() == ids.gid {
            continue;
        }
        let path = found.at.path();
        match lchown(&path, ids) {
            Ok(()) => changed += 1,
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {}
            Err(cause) => return Err(wrap(OwnerError::Set { path, cause })),
        }
    }
    Ok(changed)
}

/// The most listings a walk keeps open at once. Past it the oldest open
/// one is read to its end and closed, as walkdir's `max_open`
/// (<https://docs.rs/walkdir/2.5.0/walkdir/struct.WalkDir.html#method.max_open>),
/// whose 10 a venv's depth passes; 32 is more than any Saltbox tree
/// needs. The entry being given holds its directory's handle too (std's
/// entry keeps its listing's), so at most one more is open.
const MAX_OPEN: usize = 32;

/// The recursive walk of a root, walkdir 2.5's as `ensure_owner_recursive`
/// used it until 2026-11-20 (`follow_links(false)`,
/// `follow_root_links(false)`), entry for entry, in the same order, with
/// the same errors: the root, then depth first in the order the
/// directory lists, each directory's listing opened before the directory
/// itself is given (walkdir's `handle_entry`). No link is followed, the
/// root's included: a directory is descended into when its listed type
/// (`DirEntry::file_type`, which doesn't follow a link) says it is one.
///
/// Why not walkdir: its `DirEntry::metadata` is an `lstat` of the whole
/// path from `/` (walkdir 2.5.0, `src/dent.rs`), and it keeps no handle
/// to std's entry. std's `DirEntry::metadata` never follows a link
/// (<https://doc.rust-lang.org/std/fs/struct.DirEntry.html#method.metadata>),
/// and on glibc Linux it is a `statx`, else an `fstatat`, relative to the
/// directory already open (std 1.100.0, `library/std/src/sys/fs/unix.rs`,
/// `DirEntry::metadata`); its docs promise only `symlink_metadata`'s
/// answer, so UNVERIFIED beyond that source: on musl it is an `lstat` of
/// the path, and the walk would cost what walkdir's did. Measured on the
/// glibc build sb ships (ADR-020), over a 60,872-entry venv: 52% fewer
/// user and 38% fewer kernel instructions, 310 ms to 217 ms (performance
/// round 3, ops finding 2). An entry's path is only built when it is
/// needed: a directory to open, an entry to change, an error to name.
struct Walk {
    /// The root, until it is given.
    root: Option<PathBuf>,
    /// The listings being walked, the deepest last.
    stack: Vec<Listing>,
    /// The first listing in `stack` that may still be open: those before
    /// it are closed.
    oldest_open: usize,
    /// The most listings open at once.
    max_open: usize,
}

/// A directory's entries still to give.
enum Listing {
    /// Read from its open handle, each entry looked up relative to it.
    Open(std::fs::ReadDir),
    /// Read to its end and closed to make room ([`MAX_OPEN`]): each
    /// entry's path and listed type, looked up by its path.
    Closed(std::vec::IntoIter<Result<(PathBuf, FileType), OwnerError>>),
    /// The directory couldn't be opened: its error, given once.
    Failed(Option<OwnerError>),
}

/// An entry the walk reached, its metadata not following a link.
struct Found {
    at: At,
    metadata: Metadata,
}

/// Where a found entry is.
enum At {
    /// An entry of an open listing.
    Entry(std::fs::DirEntry),
    /// The root, or an entry of a closed listing.
    Path(PathBuf),
}

impl At {
    /// The entry's path: the root's, or its directory's joined with its
    /// name.
    fn path(&self) -> PathBuf {
        match self {
            At::Entry(entry) => entry.path(),
            At::Path(path) => path.clone(),
        }
    }
}

/// `cause` for the entry at `path`.
fn inspect(path: PathBuf, cause: io::Error) -> OwnerError {
    OwnerError::Inspect { path, cause }
}

/// A listing's read error: walkdir's carries no path (`Error::from_io`),
/// so its text names none.
fn unread(cause: io::Error) -> OwnerError {
    inspect(PathBuf::new(), cause)
}

impl Walk {
    /// The walk of `root`, at most `max_open` listings open (at least
    /// one, as walkdir's).
    fn new(root: &Path, max_open: usize) -> Self {
        Walk {
            root: Some(root.to_path_buf()),
            stack: Vec::new(),
            oldest_open: 0,
            max_open: max_open.max(1),
        }
    }

    /// `dir`'s listing opened, deepest, after the oldest open one is
    /// closed when `max_open` are.
    fn push(&mut self, dir: &Path) {
        if self.stack.len() - self.oldest_open == self.max_open {
            let oldest = &mut self.stack[self.oldest_open];
            if let Listing::Open(entries) = oldest {
                let rest: Vec<_> = entries
                    .map(|entry| {
                        let entry = entry.map_err(unread)?;
                        let path = entry.path();
                        match entry.file_type() {
                            Ok(kind) => Ok((path, kind)),
                            Err(cause) => Err(inspect(path, cause)),
                        }
                    })
                    .collect();
                *oldest = Listing::Closed(rest.into_iter());
            }
            self.oldest_open += 1;
        }
        let listing = match std::fs::read_dir(dir) {
            Ok(entries) => Listing::Open(entries),
            Err(cause) => Listing::Failed(Some(inspect(dir.to_path_buf(), cause))),
        };
        self.stack.push(listing);
    }

    /// The deepest listing done.
    fn pop(&mut self) {
        self.stack.pop();
        self.oldest_open = self.oldest_open.min(self.stack.len());
    }
}

impl Iterator for Walk {
    type Item = Result<Found, OwnerError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(root) = self.root.take() {
            let metadata = match root.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(cause) => return Some(Err(inspect(root, cause))),
            };
            if metadata.is_dir() {
                self.push(&root);
            }
            let at = At::Path(root);
            return Some(Ok(Found { at, metadata }));
        }
        loop {
            let next = match self.stack.last_mut()? {
                Listing::Open(entries) => entries.next().map(|entry| {
                    let entry = entry.map_err(unread)?;
                    match entry.file_type() {
                        Ok(kind) => Ok((At::Entry(entry), kind)),
                        Err(cause) => Err(inspect(entry.path(), cause)),
                    }
                }),
                Listing::Closed(entries) => entries
                    .next()
                    .map(|entry| entry.map(|(path, kind)| (At::Path(path), kind))),
                Listing::Failed(failure) => failure.take().map(Err),
            };
            let Some(next) = next else {
                self.pop();
                continue;
            };
            let (at, kind) = match next {
                Ok(found) => found,
                Err(failure) => return Some(Err(failure)),
            };
            if kind.is_dir() {
                self.push(&at.path());
            }
            let metadata = match &at {
                At::Entry(entry) => entry.metadata(),
                At::Path(path) => path.symlink_metadata(),
            };
            return Some(match metadata {
                Ok(metadata) => Ok(Found { at, metadata }),
                Err(cause) => Err(inspect(at.path(), cause)),
            });
        }
    }
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
        // The test's own user, by its passwd entry, on purpose: the name
        // sb would be given; a user with none fails here, saying so.
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

    /// What a walk gave: each entry's path under the root and its type
    /// (`d`, `f`, `l`: not following a link), or an error's text
    /// (`inspect ownership of <path>: <cause>`), the root's path in both
    /// as `ROOT`.
    type Walked = Vec<Result<(String, char), String>>;

    /// `path`'s record in [`Walked`], under `root`.
    fn record(root: &Path, path: &Path, metadata: &Metadata) -> (String, char) {
        let kind = metadata.file_type();
        let kind = if kind.is_symlink() {
            'l'
        } else if kind.is_dir() {
            'd'
        } else {
            'f'
        };
        let under = path.strip_prefix(root).expect("under the root");
        (format!("ROOT/{}", under.display()), kind)
    }

    /// `text` with `root`'s path as `ROOT`.
    fn rooted(root: &Path, text: &str) -> String {
        text.replace(&root.display().to_string(), "ROOT")
    }

    /// walkdir 2.5's walk as `ensure_tree` used it until 2026-11-20
    /// (`follow_links(false)`, `follow_root_links(false)`, then each
    /// entry's `metadata`), its errors as that version's text, `after`
    /// called with each entry given.
    fn walkdir_reference(root: &Path, mut after: impl FnMut(&Path)) -> Walked {
        let mut walked = Vec::new();
        let walk = walkdir::WalkDir::new(root)
            .follow_links(false)
            .follow_root_links(false);
        for entry in walk {
            match entry.and_then(|entry| entry.metadata().map(|meta| (entry, meta))) {
                Ok((entry, metadata)) => {
                    walked.push(Ok(record(root, entry.path(), &metadata)));
                    after(entry.path());
                }
                Err(error) => {
                    let path = error.path().map_or_default(Path::to_path_buf);
                    let cause = error.into_io_error().expect("an I/O error");
                    let text = format!("inspect ownership of {}: {cause}", path.display());
                    walked.push(Err(rooted(root, &text)));
                }
            }
        }
        walked
    }

    /// [`Walk`] over `root`, as [`walkdir_reference`] records it.
    fn walked(root: &Path, max_open: usize, mut after: impl FnMut(&Path)) -> Walked {
        let mut walked = Vec::new();
        for found in Walk::new(root, max_open) {
            match found {
                Ok(found) => {
                    let path = found.at.path();
                    walked.push(Ok(record(root, &path, &found.metadata)));
                    after(&path);
                }
                Err(error) => walked.push(Err(rooted(root, &error.to_string()))),
            }
        }
        walked
    }

    /// How many directories a chain under the tree goes down: more than
    /// [`MAX_OPEN`].
    const DEEP: usize = MAX_OPEN + 8;

    /// The entries [`varied_tree`] makes, the root's included: seven at
    /// the top, four under `a`, the chain's directories and its file.
    const TREE_ENTRIES: usize = 1 + 7 + 4 + DEEP + 1;

    /// A tree with a bit of everything under `root`: nested directories,
    /// files, an empty directory, symlinks to a directory, to a file and
    /// to nothing, and a chain [`DEEP`] directories deep.
    fn varied_tree(root: &Path) {
        fs::create_dir_all(root.join("a/sub")).expect("a/sub");
        for file in ["a/1", "a/2", "a/sub/x", "b"] {
            fs::write(root.join(file), "x").expect("file");
        }
        fs::create_dir(root.join("empty")).expect("empty");
        symlink(root.join("a"), root.join("link-dir")).expect("link-dir");
        symlink(root.join("b"), root.join("link-file")).expect("link-file");
        symlink(root.join("missing"), root.join("dangling")).expect("dangling");
        let mut deep = root.join("deep");
        for level in 0..DEEP {
            deep.push(format!("d{level}"));
        }
        fs::create_dir_all(&deep).expect("deep");
        fs::write(deep.join("bottom"), "x").expect("bottom");
    }

    #[test]
    fn the_walk_gives_what_walkdir_gave_in_its_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        fs::create_dir(&root).expect("root");
        varied_tree(&root);
        let reference = walkdir_reference(&root, |_| {});
        assert_eq!(reference.len(), TREE_ENTRIES, "{reference:#?}");
        assert!(reference.iter().all(Result::is_ok), "{reference:#?}");
        for max_open in [0, 1, 2, MAX_OPEN] {
            assert_eq!(
                walked(&root, max_open, |_| {}),
                reference,
                "max_open {max_open}"
            );
        }
    }

    #[test]
    fn the_walk_gives_what_walkdir_gave_for_odd_roots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        fs::create_dir(&target).expect("target");
        fs::write(target.join("inner"), "x").expect("inner");
        let file = dir.path().join("file");
        fs::write(&file, "x").expect("file");
        let link = dir.path().join("link");
        symlink(&target, &link).expect("link");
        for root in [
            link,
            file.clone(),
            dir.path().join("missing"),
            file.join("below"),
        ] {
            assert_eq!(
                walked(&root, MAX_OPEN, |_| {}),
                walkdir_reference(&root, |_| {}),
                "{}",
                root.display()
            );
        }
    }

    #[test]
    fn the_walk_keeps_at_most_max_open_listings_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        varied_tree(dir.path());
        for max_open in [1, 2, 3] {
            let mut walk = Walk::new(dir.path(), max_open);
            let mut given = 0;
            while let Some(found) = walk.next() {
                found.expect("found");
                given += 1;
                let open = walk
                    .stack
                    .iter()
                    .filter(|listing| matches!(listing, Listing::Open(_)))
                    .count();
                assert!(open <= max_open, "{open} open of at most {max_open}");
            }
            assert_eq!(given, TREE_ENTRIES);
        }
    }

    /// Two directories in a fresh `root`, in the order the directory
    /// lists them.
    fn two_directories(root: &Path) -> [PathBuf; 2] {
        for name in ["p", "q"] {
            fs::create_dir(root.join(name)).expect("dir");
        }
        let listed: Vec<PathBuf> = fs::read_dir(root)
            .expect("list")
            .map(|entry| entry.expect("entry").path())
            .collect();
        listed.try_into().expect("two entries")
    }

    /// The `after` that makes `later`, listed as a directory, a file
    /// once `earlier` is given: its open then fails, `Not a directory`.
    fn replace_after(earlier: PathBuf, later: PathBuf) -> impl FnMut(&Path) {
        move |given| {
            if given == earlier {
                fs::remove_dir(&later).expect("remove");
                fs::write(&later, "x").expect("file");
            }
        }
    }

    #[test]
    fn a_directory_that_cannot_be_opened_fails_as_walkdirs_did() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (ours, theirs) = (dir.path().join("ours"), dir.path().join("theirs"));
        let mut runs = Vec::new();
        for root in [&ours, &theirs] {
            fs::create_dir(root).expect("root");
            runs.push(two_directories(root));
        }
        let [first, second] = runs[0].clone();
        let walked = walked(&ours, MAX_OPEN, replace_after(first, second.clone()));
        let [first, second] = runs[1].clone();
        let reference = walkdir_reference(&theirs, replace_after(first, second.clone()));
        assert_eq!(walked, reference);
        let name = second.file_name().expect("name").to_string_lossy();
        assert_eq!(
            walked.last(),
            Some(&Err(format!(
                "inspect ownership of ROOT/{name}: Not a directory (os error 20)"
            )))
        );
    }

    #[test]
    fn a_directory_that_cannot_be_opened_ends_the_tree_with_its_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let [first, second] = two_directories(root);
        let mut replace = replace_after(first, second.clone());
        let error = ensure_tree(root, OTHER, &mut |path: &Path, _| {
            replace(path);
            Ok(())
        })
        .expect_err("failed");
        assert_eq!(
            error.to_string(),
            format!(
                "set recursive ownership of {}: inspect ownership of {}: Not a directory (os error 20)",
                root.display(),
                second.display()
            )
        );
    }
}
