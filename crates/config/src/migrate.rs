//! motd.yml's migration from its older format (motd spec, decision 4;
//! the maintainer, 2026-11-15): validate-config, and the validation in
//! `sb update`, rewrite an old-format motd.yml to the current one before
//! they check it.
//!
//! - **Old format:** any of the eleven app sections holds a non-empty
//!   list, Saltbox's schema up to d7eb9712 (2025-12-04, "V4"), whose
//!   template wrote `plex: [ {name, url, token} ]`. 10916e7a moved each
//!   list under `{enabled, instances}`, with the items unchanged.
//! - **Rule:** the file must pass V4's schema as it stands, each such
//!   section is wrapped as `{instances: <list>}` (`enabled` absent, which
//!   is on), and the result must pass the current schema (the Saltbox
//!   root's) with the same values. Only then is the original copied to
//!   the backups folder and the new text written in its place, with the
//!   original's owner and mode. Otherwise nothing changes, and the check
//!   reports the file as it is.
//! - **The rewrite** edits only those sections' lines: a new `instances:`
//!   line under the key, the list's lines indented under it; comments,
//!   order and spacing elsewhere stay as they are. `rutorrent:` (an
//!   older template's misspelling) and `systemd` are left alone.
//!
//! Library-backed: the parse, the tree and both schema checks are this
//! crate's validate-config path; every file step goes through rustix on
//! handles opened without following links (`openat`, `mkdirat`,
//! `fchown`, `fchmod`, `renameat`, `fsync`;
//! <https://docs.rs/rustix/1.1.6/rustix/fs/fn.openat.html>), since root
//! writes in folders the Saltbox user owns: motd.yml read, checked,
//! replaced by a temporary file renamed over it (rename(2) is atomic)
//! and its folder synced, all through the Saltbox root's handle; the
//! backup made through the backups folder's. The backup's name from
//! jiff's UTC time. Why not a YAML emitter (saphyr's, serde-saphyr's):
//! it writes the whole document again and drops the comments, where the
//! rule keeps the file as written; no format-keeping YAML editor is in
//! the tree (`yaml-edit` and the like have little use). V4's schema is
//! kept in the binary: Saltbox's checkout holds only the current one.

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;
use sb_cli_core::config::{MAX_FILE_BYTES, yaml};

use crate::issue::escape_controls;
use crate::run::{self, FILES, File, Parse, Parsed, Settings};
use crate::tree::{self, Data, Node};
use crate::{parse, schema};

/// Where the original is copied before the rewrite, unless the caller
/// names another folder (sb's `SB_BACKUPS_DIR`, the tests').
pub const BACKUPS: &str = "/opt/saltbox/backups";

/// The backups folder when the caller names none: [`BACKUPS`], but in a
/// build for tests (this crate's own, or with its `testing` feature, as
/// sb's test build has, never a release: check.sh checks) none at all, so
/// no test can reach the host's, even one whose `SB_BACKUPS_DIR` a mutant
/// of the gate's mutation pass drops.
#[must_use]
pub fn default_backups() -> PathBuf {
    if cfg!(any(test, feature = "testing")) {
        // Empty: the backup is refused, so a test that forgets its folder
        // fails rather than writes anywhere shared.
        PathBuf::new()
    } else {
        PathBuf::from(BACKUPS)
    }
}

/// motd.yml and its schema (sb-config's list; a test checks the entry).
pub(crate) const MOTD: File = FILES[5];

/// The app sections that held a bare list in the old format.
pub(crate) const APPS: [&str; 11] = [
    "sonarr",
    "radarr",
    "lidarr",
    "readarr",
    "plex",
    "jellyfin",
    "emby",
    "sabnzbd",
    "nzbget",
    "qbittorrent",
    "rtorrent",
];

/// Saltbox's motd schema at d7eb9712, the last one with bare lists
/// (<https://github.com/saltyorg/Saltbox/blob/d7eb9712/schema/motd.schema.yml>).
const OLD_SCHEMA: &str = include_str!("../schemas/motd-d7eb9712.schema.yml");

/// What the checks decided about a motd.yml text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Not the old format (or not a file that parses): checked as it is.
    Current,
    /// The old format, but a check failed or the rewrite couldn't be
    /// made: why, for the run log. Checked as it is.
    Kept(&'static str),
    /// The old format, both checks passed: the new text.
    Migrate(String),
}

/// The plan for `text`, motd.yml's, with the current schema from
/// `settings`' Saltbox root. A file with no app section holding a list
/// is current, decided from its plain tree before the older schema is
/// parsed and checked, which cost most of the plan (the performance pass,
/// 2026-11-18: about 3 M instructions, 13% of validate-config). The
/// decision is [`plan_checked`]'s: a file it would take further has such
/// a section in the plain tree too, whose shape the secret paths don't
/// change (they only redact an issue's text). For a current file, the
/// parse the decision made comes too, for the file's check.
#[must_use]
pub(crate) fn plan(settings: &Settings, text: &str) -> (Plan, Option<Parse>) {
    let (lists, parse) = lists_apps(text);
    if !lists {
        return (Plan::Current, Some(parse));
    }
    (plan_checked(settings, text), None)
}

/// Whether `text` parses into a tree with an app section holding a
/// non-empty list, the old format's mark: [`run::parse_and_check`]'s
/// steps without a schema, on the YAML reader's stack (`yaml::with_stack`),
/// where the tree is dropped too. A file that doesn't parse, or whose
/// reader can't start, has none, as `plan_checked` reads it. Its pre-pass
/// and load come back for the check, the load only where the caller's
/// stack holds its tree.
fn lists_apps(text: &str) -> (bool, Parse) {
    let prepared = parse::prepare(MOTD.path, text);
    let lists = |root: &saphyr::MarkedYamlOwned| {
        tree::build(MOTD.path, root, &|_| false).is_ok_and(|tree| !old_sections(&tree).is_empty())
    };
    let (lists, loaded) = match &prepared {
        Err(_) => (false, None),
        Ok(prepared) if prepared.depth() <= yaml::SHALLOW => {
            let loaded = parse::load(MOTD.path, prepared);
            (loaded.as_ref().is_ok_and(lists), Some(loaded))
        }
        Ok(prepared) => {
            let lists = yaml::with_stack(prepared.depth(), || {
                parse::load(MOTD.path, prepared).is_ok_and(|root| lists(&root))
            })
            .unwrap_or(false);
            (lists, None)
        }
    };
    (lists, Parse { prepared, loaded })
}

/// The plan for `text` with every check: the older schema's, and the
/// rewrite's against the current schema.
#[must_use]
fn plan_checked(settings: &Settings, text: &str) -> Plan {
    let old_rules = schema::parse("motd-d7eb9712.schema.yml", OLD_SCHEMA)
        .expect("the pinned older schema loads (a test loads it)");
    let Ok(old) = run::parse_and_check(settings, MOTD, text, Some(&old_rules)) else {
        return Plan::Current;
    };
    let Some(old_tree) = old.tree else {
        return Plan::Current;
    };
    let sections = old_sections(&old_tree);
    if sections.is_empty() {
        return Plan::Current;
    }
    if !old.report.issues.is_empty() {
        return Plan::Kept("it doesn't pass the older schema");
    }
    let Some(new_text) = rewrite(text, &old_tree, &sections) else {
        return Plan::Kept("its lists can't be rewritten in place");
    };
    let path = settings
        .root
        .join("schema")
        .join(MOTD.schema.unwrap_or_default());
    let shown = escape_controls(&path.display().to_string());
    let Ok(rules) = schema::load(&path, &shown) else {
        return Plan::Kept("the current schema doesn't load");
    };
    let Ok(new) = run::parse_and_check(settings, MOTD, &new_text, Some(&rules)) else {
        return Plan::Kept("the rewritten file can't be read");
    };
    if !new.report.issues.is_empty() {
        return Plan::Kept("the rewritten file doesn't pass the current schema");
    }
    match new.tree {
        Some(new_tree) if same_values(&old_tree, &new_tree, &sections) => Plan::Migrate(new_text),
        _ => Plan::Kept("the rewritten file doesn't hold the same values"),
    }
}

/// The app sections of `root` that hold a non-empty list.
fn old_sections(root: &Node) -> Vec<&'static str> {
    APPS.into_iter()
        .filter(|app| matches!(&root.get(app).map(|node| &node.data), Some(Data::Sequence(items)) if !items.is_empty()))
        .collect()
}

/// `text` with each of `sections` (top-level keys of `root`, the tree
/// parsed from it) wrapped as `{instances: <list>}`: a new `  instances:`
/// line under the key, and the list's lines indented under it. None when
/// a key isn't at the start of its line or its value is on the key's line
/// in a form this doesn't rewrite.
fn rewrite(text: &str, root: &Node, sections: &[&str]) -> Option<String> {
    let Data::Mapping(entries) = &root.data else {
        return None;
    };
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    // From the last section to the first, so earlier line numbers hold.
    let mut found: Vec<(usize, usize, usize, usize)> = Vec::new();
    for (index, (key, value)) in entries.iter().enumerate() {
        if !sections.contains(&key.text.as_str()) {
            continue;
        }
        if key.column != 1 {
            return None;
        }
        let end = entries
            .get(index + 1)
            .map_or(lines.len() + 1, |(next, _)| next.line);
        found.push((key.line, value.line, value.column, end));
    }
    for (key_line, value_line, value_column, end) in found.into_iter().rev() {
        let key_at = key_line.checked_sub(1)?;
        let ending = if lines.get(key_at)?.ends_with("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        if value_line == key_line {
            // A flow list (or an alias) on the key's line: the value
            // moves to an `instances:` line of its own, and any line it
            // runs on to is indented with it.
            let line = lines.get(key_at)?;
            let at = line.char_indices().nth(value_column - 1)?.0;
            let (head, rest) = line.split_at(at);
            let rest = rest.to_string();
            lines[key_at] = format!("{}{ending}  instances: {rest}", head.trim_end());
            indent(&mut lines, key_line, end - 1, 2);
        } else {
            // A block list below the key: its items at their own column
            // (the first line of the list that isn't blank or a comment),
            // which moves under a new `instances:` at two spaces, the
            // items at four or deeper.
            let column = lines
                .iter()
                .take(end - 1)
                .skip(key_line)
                .map(|line| line.trim_end_matches(['\r', '\n']))
                .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
                .map_or(0, |line| line.len() - line.trim_start_matches(' ').len());
            let by = 4_usize.saturating_sub(column).max(2);
            indent(&mut lines, key_line, end - 1, by);
            lines.insert(key_line, format!("  instances:{ending}"));
        }
    }
    Some(lines.concat())
}

/// Lines `from..to` (0-based, `to` excluded) indented by `by` spaces,
/// but for blank lines and comments at the start of a line, which may
/// belong to the next key and are valid anywhere.
fn indent(lines: &mut [String], from: usize, to: usize, by: usize) {
    let pad = " ".repeat(by);
    for line in lines.iter_mut().take(to).skip(from) {
        let bare = line.trim_end_matches(['\r', '\n']);
        if bare.trim().is_empty() || bare.starts_with('#') {
            continue;
        }
        *line = format!("{pad}{line}");
    }
}

/// Whether `new` holds `old`'s values, each of `sections` now under
/// `instances`: positions aside, each scalar's text as written too. Walked from a list on
/// the heap, so a deep file costs no stack.
fn same_values(old: &Node, new: &Node, sections: &[&str]) -> bool {
    let (Data::Mapping(old_entries), Data::Mapping(new_entries)) = (&old.data, &new.data) else {
        return false;
    };
    if old_entries.len() != new_entries.len() {
        return false;
    }
    let mut pending: Vec<(&Node, &Node)> = Vec::new();
    for ((old_key, old_value), (new_key, new_value)) in old_entries.iter().zip(new_entries) {
        if old_key.text != new_key.text {
            return false;
        }
        let new_value = if sections.contains(&old_key.text.as_str()) {
            match &new_value.data {
                Data::Mapping(inner) if inner.len() == 1 => match inner.first() {
                    Some((key, list)) if key.text == "instances" => list,
                    _ => return false,
                },
                _ => return false,
            }
        } else {
            new_value
        };
        pending.push((old_value, new_value));
    }
    while let Some((a, b)) = pending.pop() {
        match (&a.data, &b.data) {
            (Data::Scalar(x), Data::Scalar(y)) if x == y && a.source == b.source => {}
            (Data::Sequence(x), Data::Sequence(y)) if x.len() == y.len() => {
                pending.extend(x.iter().zip(y));
            }
            (Data::Mapping(x), Data::Mapping(y)) if x.len() == y.len() => {
                for ((xk, xv), (yk, yv)) in x.iter().zip(y) {
                    if xk.text != yk.text {
                        return false;
                    }
                    pending.push((xv, yv));
                }
            }
            _ => return false,
        }
    }
    true
}

/// motd.yml under `settings`' root migrated when its [`plan`] says so:
/// the original copied into `backups`, then the new text written in its
/// place atomically, with the original's owner and mode. The backup's
/// path when it was migrated; none when there is no file, it isn't a
/// regular file, or the plan keeps it (the reason logged). A file in the
/// current format leaves its text and parse in `parsed`, for its check.
///
/// # Errors
///
/// The text shown when the file was to be migrated and the backup or the
/// write failed, or the file changed meanwhile; the original is then
/// unchanged.
pub(crate) fn motd(
    settings: &Settings,
    backups: &Path,
    parsed: &mut Option<Parsed>,
) -> Result<Option<PathBuf>, String> {
    let stamp = jiff::Timestamp::now()
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string();
    motd_stamped(settings, backups, &stamp, parsed)
}

/// [`motd`] with the backup's stamp given (the tests').
fn motd_stamped(
    settings: &Settings,
    backups: &Path,
    stamp: &str,
    parsed: &mut Option<Parsed>,
) -> Result<Option<PathBuf>, String> {
    // The Saltbox root as one handle, not followed through a link: every
    // step on motd.yml goes through it, so the folder can't be swapped
    // for a link meanwhile (its parent may be the Saltbox user's).
    // In a build for tests, never the host's checkout, which a mutant of
    // the gate's mutation pass dropping `SB_SALTBOX_DIR` would point at.
    if cfg!(any(test, feature = "testing")) && settings.root.starts_with("/srv") {
        tracing::warn!("motd.yml not migrated: a test build doesn't write under /srv");
        return Ok(None);
    }
    let Ok(root) = open_folder(&settings.root) else {
        return Ok(None);
    };
    let Some(original) = Original::read(&root) else {
        return Ok(None);
    };
    let new_text = match plan(settings, &original.text) {
        (Plan::Current, current) => {
            *parsed = current.map(|parse| Parsed {
                text: original.text,
                parse,
            });
            return Ok(None);
        }
        (Plan::Kept(why), _) => {
            tracing::info!(why, "motd.yml in the older format, not migrated");
            return Ok(None);
        }
        (Plan::Migrate(new_text), _) => new_text,
    };
    let backup = back_up(&original, backups, stamp)?;
    let path = settings.root.join(MOTD.path);
    write(&root, &new_text, &original, stamp)
        .map_err(|err| format!("{}: {err}", escape_controls(&path.display().to_string())))?;
    tracing::info!(backup = %backup.display(), "motd.yml migrated to the current format");
    Ok(Some(backup))
}

/// motd.yml as it was read: its text, owner and mode, and what tells
/// whether it has changed since.
struct Original {
    text: String,
    owner: (u32, u32),
    /// The permission bits alone: no set-id or sticky bit is carried.
    mode: u32,
    seen: (u64, u64, i64, i64, u64),
}

impl Original {
    /// motd.yml in `root` opened once without following a link, checked
    /// to be a regular file with one name (a hard link could be any file
    /// on the filesystem) through that handle, and read from it up to
    /// cli-core's 1 MiB; none for anything else (the check reports it).
    fn read(root: &OwnedFd) -> Option<Self> {
        let fd = rustix::fs::openat(
            root,
            MOTD.path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .ok()?;
        let stat = rustix::fs::fstat(&fd).ok()?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            tracing::info!("motd.yml not migrated: not a regular file with one name");
            return None;
        }
        let mut bytes = Vec::new();
        let cap = u64::try_from(MAX_FILE_BYTES).unwrap_or(u64::MAX);
        fs::File::from(fd)
            .take(cap.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_FILE_BYTES {
            return None;
        }
        Some(Self {
            text: String::from_utf8(bytes).ok()?,
            owner: (stat.st_uid, stat.st_gid),
            mode: stat.st_mode & 0o777,
            seen: seen(&stat),
        })
    }

    /// Whether motd.yml in `root` is still the file read: the same inode,
    /// size and modification time, not followed through a link.
    fn unchanged(&self, root: &OwnedFd) -> bool {
        rustix::fs::statat(root, MOTD.path, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|stat| seen(&stat) == self.seen)
    }
}

/// What tells a file apart from its next version: device, inode, size
/// and modification time.
fn seen(stat: &rustix::fs::Stat) -> (u64, u64, i64, i64, u64) {
    (
        stat.st_dev,
        stat.st_ino,
        stat.st_size,
        stat.st_mtime,
        stat.st_mtime_nsec,
    )
}

/// The original's text copied to `<backups>/motd.yml.<stamp>`, with its
/// owner and mode, through a handle on the folder ([`backups_folder`]): a
/// new file (`O_EXCL`, never through a link), removed again if any part
/// of the copy fails, then the file and the folder synced
/// (<https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_all>).
fn back_up(original: &Original, backups: &Path, stamp: &str) -> Result<PathBuf, String> {
    let shown = |path: &Path| escape_controls(&path.display().to_string());
    let folder = backups_folder(backups, original.owner)
        .map_err(|err| format!("{}: {err}", shown(backups)))?;
    let name = format!("motd.yml.{stamp}");
    let backup = backups.join(&name);
    let failed = |err: std::io::Error| format!("{}: {err}", shown(&backup));
    let fd = rustix::fs::openat(
        &folder,
        name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(original.mode),
    )
    .map_err(|errno| failed(errno.into()))?;
    let mut file = fs::File::from(fd);
    let copied = file
        .write_all(original.text.as_bytes())
        .and_then(|()| {
            std::os::unix::fs::fchown(&file, Some(original.owner.0), Some(original.owner.1))
        })
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(original.mode)))
        .and_then(|()| file.sync_all());
    if let Err(err) = copied {
        let _ = rustix::fs::unlinkat(&folder, name.as_str(), AtFlags::empty());
        return Err(failed(err));
    }
    fs::File::from(folder).sync_all().map_err(failed)?;
    Ok(backup)
}

/// `folder` opened with no link anywhere on its path (openat2's
/// `RESOLVE_NO_SYMLINKS`, Linux 5.6, below every supported Ubuntu;
/// <https://docs.rs/rustix/1.1.6/rustix/fs/fn.openat2.html>): `O_NOFOLLOW`
/// guards only the last part, and the Saltbox user may own a folder above
/// it. A link is ELOOP.
fn open_folder(folder: &Path) -> rustix::io::Result<OwnedFd> {
    rustix::fs::openat2(
        rustix::fs::CWD,
        folder,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
}

/// Whether the folder `fd` holds nothing but `.` and `..`.
fn empty(fd: &OwnedFd) -> std::io::Result<bool> {
    for entry in rustix::fs::Dir::read_from(fd)? {
        let name = entry?.file_name().to_bytes().to_vec();
        if name != b"." && name != b".." {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The backups folder, opened without following a link, as a handle the
/// backup is made in.
/// - Missing: its parents made as root's (0755), then the folder itself,
///   each through its parent's handle; the folder given motd.yml's owner
///   and 0755 through its own handle.
/// - There: it must be a folder (a link isn't followed), owned by root or
///   by motd.yml's owner, and writable by no one else.
///
/// Why not std's `DirBuilder::recursive`: it doesn't say which folders it
/// made, so the new one can't be given its owner, and it follows a linked
/// ancestor; why not paths: a folder swapped for a link between the check
/// and the use would be followed, by root.
fn backups_folder(folder: &Path, owner: (u32, u32)) -> std::io::Result<OwnedFd> {
    let dir = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match open_folder(folder) {
        Ok(fd) => {
            let stat = rustix::fs::fstat(&fd)?;
            let others = stat.st_mode & 0o022 != 0;
            if !(stat.st_uid == 0 || stat.st_uid == owner.0) || others {
                return Err(std::io::Error::other(
                    "not owned by root or motd.yml's owner, or writable by others; not used",
                ));
            }
            Ok(fd)
        }
        Err(Errno::NOENT) => {
            let (Some(parent), Some(name)) = (folder.parent(), folder.file_name()) else {
                return Err(std::io::Error::other("no parent folder"));
            };
            let parent = parent_folder(parent)?;
            // One made by someone else meanwhile isn't used.
            rustix::fs::mkdirat(&parent, name, Mode::from_raw_mode(0o755))?;
            let fd = rustix::fs::openat(&parent, name, dir, Mode::empty())?;
            let stat = rustix::fs::fstat(&fd)?;
            // Still the empty folder just made, not one renamed into its
            // place (its link count says nothing on btrfs).
            if stat.st_uid != rustix::process::geteuid().as_raw() || !empty(&fd)? {
                return Err(std::io::Error::other(
                    "made by someone else meanwhile; not used",
                ));
            }
            rustix::fs::fchown(
                &fd,
                Some(rustix::process::Uid::from_raw(owner.0)),
                Some(rustix::process::Gid::from_raw(owner.1)),
            )?;
            // The process's umask may have taken bits off 0755.
            rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o755))?;
            Ok(fd)
        }
        Err(Errno::LOOP | Errno::NOTDIR) => Err(std::io::Error::other(
            "a link or not a folder, or a link on its path; not followed",
        )),
        Err(errno) => Err(errno.into()),
    }
}

/// `folder` opened, made first (with any missing parent) as root's,
/// 0755, through each parent's handle when it is missing.
fn parent_folder(folder: &Path) -> std::io::Result<OwnedFd> {
    let dir = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    match open_folder(folder) {
        Err(Errno::NOENT) => {
            let (Some(parent), Some(name)) = (folder.parent(), folder.file_name()) else {
                return Err(std::io::Error::other("no parent folder"));
            };
            let parent = parent_folder(parent)?;
            match rustix::fs::mkdirat(&parent, name, Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(errno) => return Err(errno.into()),
            }
            let fd = rustix::fs::openat(&parent, name, dir | OFlags::NOFOLLOW, Mode::empty())?;
            rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o755))?;
            Ok(fd)
        }
        opened => Ok(opened?),
    }
}

/// `text` written over motd.yml in `root` atomically: a new temporary
/// file beside it (`O_EXCL`, never through a link), given the original's
/// owner and mode and synced, renamed over it only if it is still the
/// file read, then the folder synced; the temporary file removed on any
/// failure. Why not tempfile's `NamedTempFile`: it works by path, where
/// root must stay on the folder's handle (the Saltbox user may own its
/// parent), and it has no constructor for a folder handle.
fn write(root: &OwnedFd, text: &str, original: &Original, stamp: &str) -> std::io::Result<()> {
    let temporary = format!(".{}.{}.{stamp}.sb-tmp", MOTD.path, std::process::id());
    let fd = rustix::fs::openat(
        root,
        temporary.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?;
    let mut file = fs::File::from(fd);
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| {
            std::os::unix::fs::fchown(&file, Some(original.owner.0), Some(original.owner.1))
        })
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(original.mode)))
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            if original.unchanged(root) {
                Ok(())
            } else {
                Err(std::io::Error::other(
                    "changed while it was checked; not migrated",
                ))
            }
        })
        .and_then(|()| {
            Ok(rustix::fs::renameat(
                root,
                temporary.as_str(),
                root,
                MOTD.path,
            )?)
        });
    if let Err(err) = written {
        let _ = rustix::fs::unlinkat(root, temporary.as_str(), AtFlags::empty());
        return Err(err);
    }
    Ok(rustix::fs::fsync(root)?)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::http::{Http, Proxy};

    /// A Saltbox root holding the pinned current schema and `motd`.
    fn root(motd: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("root");
        fs::create_dir(root.path().join("schema")).expect("schema dir");
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/saltbox-schemas/motd.schema.yml"),
            root.path().join("schema/motd.schema.yml"),
        )
        .expect("schema");
        fs::write(root.path().join("motd.yml"), motd).expect("motd.yml");
        root
    }

    fn settings(root: &Path) -> Settings {
        Settings {
            root: root.to_path_buf(),
            zoneinfo: Vec::new(),
            http: Http::new(&Proxy::Off, Duration::from_secs(1)).expect("client"),
            cloudflare_url: String::new(),
            dockerhub_url: String::new(),
            backoff: crate::cloudflare::backoff,
        }
    }

    /// A current motd.yml's text and parse go to its check; a migrated
    /// one's don't (its check reads the new text), nor do a file's that
    /// the plan keeps in the older format.
    #[test]
    fn a_current_files_parse_goes_to_its_check() {
        let current = "plex:\n  enabled: true\n  instances:\n    - name: Plex\n      url: http://plex:32400\n      token: t\n";
        let root = root(current);
        let backups = root.path().join("backups");
        let mut parsed = None;
        assert_eq!(
            motd_stamped(&settings(root.path()), &backups, "s", &mut parsed),
            Ok(None)
        );
        let parsed = parsed.expect("handed on");
        assert_eq!(parsed.text, current);
        assert!(matches!(parsed.parse.loaded, Some(Ok(_))));
        // Block mappings: saphyr's flow level stops at 255.
        let mut deep = String::new();
        for level in 0..=yaml::SHALLOW {
            deep.push_str(&" ".repeat(level));
            deep.push_str("k:\n");
        }
        let root = self::root(&deep);
        let mut parsed = None;
        assert_eq!(
            motd_stamped(&settings(root.path()), &backups, "s", &mut parsed),
            Ok(None)
        );
        let parsed = parsed.expect("handed on");
        assert!(parsed.parse.prepared.is_ok() && parsed.parse.loaded.is_none());
        for kept in [T1, "plex:\n  - name: Plex\n"] {
            let root = self::root(kept);
            let mut parsed = None;
            let _ = motd_stamped(
                &settings(root.path()),
                &root.path().join("backups"),
                "s",
                &mut parsed,
            );
            assert!(parsed.is_none(), "{kept:?}");
        }
    }

    fn plan_of(motd: &str) -> Plan {
        let root = root(motd);
        plan(&settings(root.path()), motd).0
    }

    /// A file as Saltbox's list-era template wrote it (lists with url and
    /// credential, `[]` for an app not installed), with comments, a blank
    /// line and the colours: each list moves under `instances`, all else
    /// stays as written.
    const T1: &str = "---\n# Saltbox MOTD\nlidarr:\n  []\nplex:\n  - name: Plex\n    url: http://plex:32400\n    token: plex-token-zz # the token\n\n# Radarr's instances\nradarr:\n  - name: Radarr\n    url: http://radarr:7878\n    apikey: radarr-key-zz\n  # a comment in the list\n  - name: Radarr 4K\n    url: http://radarr4k:7878\n    apikey: radarr4k-key-zz\n    timeout: 2\ncolors:\n  text:\n    label: '#FF79D0'\n";

    #[test]
    fn a_list_era_file_is_rewritten_keeping_its_comments() {
        let Plan::Migrate(new) = plan_of(T1) else {
            panic!("{:?}", plan_of(T1));
        };
        assert_eq!(
            new,
            "---\n# Saltbox MOTD\nlidarr:\n  []\nplex:\n  instances:\n    - name: Plex\n      url: http://plex:32400\n      token: plex-token-zz # the token\n\n# Radarr's instances\nradarr:\n  instances:\n    - name: Radarr\n      url: http://radarr:7878\n      apikey: radarr-key-zz\n    # a comment in the list\n    - name: Radarr 4K\n      url: http://radarr4k:7878\n      apikey: radarr4k-key-zz\n      timeout: 2\ncolors:\n  text:\n    label: '#FF79D0'\n"
        );
    }

    /// Lists at the key's column, in flow style, with CRLF line ends, and
    /// the last section in the file.
    #[test]
    fn other_list_layouts_are_rewritten() {
        let compact = "sonarr:\n- name: Sonarr\n  url: http://sonarr:8989\n  apikey: k1\n";
        assert_eq!(
            plan_of(compact),
            Plan::Migrate(
                "sonarr:\n  instances:\n    - name: Sonarr\n      url: http://sonarr:8989\n      apikey: k1\n"
                    .to_string()
            )
        );
        let flow = "emby: [{name: Emby, url: 'http://emby:8096', token: t1}] # one\nplex: []\n";
        assert_eq!(
            plan_of(flow),
            Plan::Migrate(
                "emby:\n  instances: [{name: Emby, url: 'http://emby:8096', token: t1}] # one\nplex: []\n"
                    .to_string()
            )
        );
        let crlf = "plex:\r\n  - name: Plex\r\n    url: http://plex:32400\r\n    token: t\r\n";
        assert_eq!(
            plan_of(crlf),
            Plan::Migrate(
                "plex:\r\n  instances:\r\n    - name: Plex\r\n      url: http://plex:32400\r\n      token: t\r\n"
                    .to_string()
            )
        );
        let deep = "nzbget:\n      - name: N\n        url: http://nzbget:6789\n        user: u\n        password: p\n";
        assert_eq!(
            plan_of(deep),
            Plan::Migrate(
                "nzbget:\n  instances:\n        - name: N\n          url: http://nzbget:6789\n          user: u\n          password: p\n"
                    .to_string()
            )
        );
    }

    /// The current form, an empty list, no app section, and a file that
    /// doesn't parse are left to the check as they are.
    #[test]
    fn a_current_file_is_left_alone() {
        for motd in [
            "plex:\n  instances:\n    - name: Plex\n      url: http://plex:32400\n      token: t\n",
            "plex: []\nradarr:\n  []\n",
            "colors:\n  text:\n    label: '#FF79D0'\n",
            "plex: [\n",
            "",
        ] {
            assert_eq!(plan_of(motd), Plan::Current, "{motd}");
        }
    }

    /// An old file that doesn't pass V4 (an item without its url, an
    /// unknown key such as an older template's `rutorrent:`, V3's
    /// `strip_prefixes`) is kept: nothing changes, and the check
    /// reports it.
    #[test]
    fn an_old_file_that_fails_the_older_schema_is_kept() {
        for motd in [
            "plex:\n  - name: Plex\n    token: t\n",
            "plex:\n  - name: Plex\n    url: http://plex:32400\n    token: t\nrutorrent:\n  - name: R\n",
            "plex:\n  - name: Plex\n    url: http://plex:32400\n    token: t\nsystemd:\n  strip_prefixes: [saltbox_managed_]\n",
        ] {
            assert_eq!(
                plan_of(motd),
                Plan::Kept("it doesn't pass the older schema"),
                "{motd}"
            );
        }
    }

    /// A rewrite the current schema refuses, or one whose values differ,
    /// or a key not at the start of its line, is kept.
    #[test]
    fn a_rewrite_that_fails_is_kept() {
        // The current schema without `instances` for plex refuses it.
        let root = root("");
        fs::write(
            root.path().join("schema/motd.schema.yml"),
            "plex:\n  type: object\n  properties:\n    enabled: {type: boolean}\n",
        )
        .expect("schema");
        let old = "plex:\n  - name: Plex\n    url: http://plex:32400\n    token: t\n";
        assert_eq!(
            plan(&settings(root.path()), old).0,
            Plan::Kept("the rewritten file doesn't pass the current schema")
        );
        fs::write(
            root.path().join("schema/motd.schema.yml"),
            "plex:\n  kind: x\n",
        )
        .expect("schema");
        assert_eq!(
            plan(&settings(root.path()), old).0,
            Plan::Kept("the current schema doesn't load")
        );
        // A document indented as a whole: its keys aren't at column 1.
        assert_eq!(
            plan_of("  plex:\n    - name: Plex\n      url: http://plex:32400\n      token: t\n"),
            Plan::Kept("its lists can't be rewritten in place")
        );
        // A literal block in an item moves with it, its value kept.
        let block = "plex:\n  - name: |\n      Plex\n    url: http://plex:32400\n    token: t\n";
        assert!(
            matches!(plan_of(block), Plan::Migrate(_)),
            "{:?}",
            plan_of(block)
        );
    }

    /// The quick format check decides as the full one did (the
    /// performance pass, 2026-11-18): every text the tests here use, the
    /// fixture set's motd.yml, and texts the tree build or the parser treat
    /// apart (a key that is a mapping, a bad tag, one under a secret key,
    /// an alias, a duplicate key, nesting past the reader's shallow stack,
    /// a second document) give the same plan both ways, and the quick
    /// check passes exactly the files the full one takes past current.
    #[test]
    fn the_quick_check_decides_as_the_full_one() {
        let valid = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/valid/motd.yml"),
        )
        .expect("the valid fixture");
        let deep = (0..300).fold(
            String::from("plex:\n  - name: P\n    url: http://p\n    token: t\n    x:\n"),
            |text, i| format!("{text}{}k:\n", "  ".repeat(i + 3)),
        );
        assert!(
            parse::prepare(MOTD.path, &deep).expect("prepares").depth() > yaml::SHALLOW,
            "the deep text is read on the reader's own thread"
        );
        let texts = [
            T1,
            valid.as_str(),
            deep.as_str(),
            "sonarr:\n- name: Sonarr\n  url: http://sonarr:8989\n  apikey: k1\n",
            "emby: [{name: Emby, url: 'http://emby:8096', token: t1}] # one\nplex: []\n",
            "plex:\r\n  - name: Plex\r\n    url: http://plex:32400\r\n    token: t\r\n",
            "nzbget:\n      - name: N\n        url: http://nzbget:6789\n        user: u\n        password: p\n",
            "plex:\n  instances:\n    - name: Plex\n      url: http://plex:32400\n      token: t\n",
            "plex: []\nradarr:\n  []\n",
            "colors:\n  text:\n    label: '#FF79D0'\n",
            "plex: [\n",
            "",
            "plex:\n  - name: Plex\n    token: t\n",
            "plex:\n  - name: Plex\n    url: http://plex:32400\n    token: t\nrutorrent:\n  - name: R\n",
            "  plex:\n    - name: Plex\n      url: http://plex:32400\n      token: t\n",
            "plex:\n  - name: |\n      Plex\n    url: http://plex:32400\n    token: t\n",
            "? {a: 1}\n: x\nplex:\n  - name: P\n    url: http://p\n    token: t\n",
            "plex:\n  - name: !!int nope\n    url: http://p\n    token: t\n",
            "plex:\n  - name: P\n    url: http://p\n    token: !!int nope\n",
            "base: &b\n  - name: P\n    url: http://p\n    token: t\nplex: *b\n",
            "plex:\n  - name: P\nplex:\n  - name: Q\n",
            "plex:\n  - name: P\n    url: http://p\n    token: t\n---\nplex: []\n",
            "sonarr: 3\nradarr: {a: 1}\n",
        ];
        let root = root("");
        let settings = settings(root.path());
        for text in texts {
            assert_eq!(
                plan(&settings, text).0,
                plan_checked(&settings, text),
                "{text:?}"
            );
            assert_eq!(
                lists_apps(text).0,
                plan_checked(&settings, text) != Plan::Current,
                "{text:?}"
            );
        }
    }

    #[test]
    fn values_are_compared_whole() {
        let tree = |text: &str| {
            let root = parse::parse("motd.yml", text).expect("parses");
            tree::build("motd.yml", &root, &|_| true).expect("builds")
        };
        let old = tree("plex:\n  - name: a\n");
        let new = tree("plex:\n  instances:\n    - name: a\n");
        assert!(same_values(&old, &new, &["plex"]));
        for other in [
            "plex:\n  instances:\n    - name: b\n",
            "plex:\n  instances:\n    - name: a\n  enabled: true\n",
            "plex:\n  other:\n    - name: a\n",
            "plex:\n  instances:\n    - name: a\n      url: x\n",
            "plex:\n  instances:\n    - name: a\n    - name: b\n",
            "plex:\n  instances: x\n",
            "emby:\n  instances:\n    - name: a\n",
            "plex:\n  instances:\n    - name: a\nemby: []\n",
            "plex:\n  instances:\n    - other: a\n",
            "plex:\n  instances:\n    - [a]\n",
        ] {
            assert!(!same_values(&old, &tree(other), &["plex"]), "{other}");
        }
        // A root that isn't a mapping (built by hand: no file parses so).
        let list = Node {
            line: 1,
            column: 1,
            data: Data::Sequence(Vec::new()),
            source: None,
        };
        assert!(!same_values(&list, &new, &["plex"]));
        assert!(!same_values(&old, &list, &["plex"]));
    }

    /// A test build never defaults to a backups folder, and never
    /// migrates a file under /srv (the host's checkout).
    #[test]
    fn a_test_build_has_no_default_folder_and_skips_srv() {
        assert_eq!(default_backups(), PathBuf::new());
        assert_eq!(BACKUPS, "/opt/saltbox/backups");
        let root = root(T1);
        let err = motd_stamped(&settings(root.path()), &default_backups(), "s", &mut None)
            .expect_err("no folder");
        assert!(err.ends_with("no parent folder"), "{err}");
        assert_eq!(
            fs::read_to_string(root.path().join("motd.yml")).expect("read"),
            T1
        );
        let mut host = settings(root.path());
        host.root = PathBuf::from("/srv/git/saltbox");
        assert_eq!(
            motd_stamped(&host, &root.path().join("backups"), "s", &mut None),
            Ok(None)
        );
    }

    #[test]
    fn the_file_is_motd_yml() {
        assert_eq!(MOTD.path, "motd.yml");
        assert_eq!(MOTD.schema, Some("motd.schema.yml"));
        assert!(schema::parse("motd-d7eb9712.schema.yml", OLD_SCHEMA).is_ok());
    }

    /// A half-migrated file (one section already in the current form) is
    /// finished: V4 doesn't check a mapping where it wants a list, and the
    /// rewrite passes the current schema.
    #[test]
    fn a_half_migrated_file_is_finished() {
        let mixed = "plex:\n  instances:\n    - name: Plex\n      url: http://plex:32400\n      token: t\nsonarr:\n  - name: Sonarr\n    url: http://sonarr:8989\n    apikey: k\n";
        assert_eq!(
            plan_of(mixed),
            Plan::Migrate(
                "plex:\n  instances:\n    - name: Plex\n      url: http://plex:32400\n      token: t\nsonarr:\n  instances:\n    - name: Sonarr\n      url: http://sonarr:8989\n      apikey: k\n"
                    .to_string()
            )
        );
    }

    /// Whether we run as root: a file we make is ours.
    fn as_root(path: &Path) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        fs::metadata(path).expect("meta").uid() == 0
    }

    /// The whole migration: the backup is the original, with its owner
    /// and mode (0640: neither tempfile's 0600 nor a umask's 0644), in a
    /// folder made 0755 with the file's owner, its missing parents root's;
    /// the new file is written atomically (a new inode, no temporary
    /// left) with the original's owner and mode.
    #[test]
    fn the_file_is_backed_up_then_replaced() {
        use std::os::unix::fs::MetadataExt as _;
        let root = root(T1);
        let path = root.path().join("motd.yml");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("chmod");
        let me = {
            let meta = fs::metadata(&path).expect("meta");
            (meta.uid(), meta.gid())
        };
        // As root, a Saltbox user's file; otherwise the test's own.
        let owner = if as_root(&path) {
            std::os::unix::fs::chown(&path, Some(4321), Some(4322)).expect("chown");
            (4321, 4322)
        } else {
            me
        };
        let inode = fs::metadata(&path).expect("meta").ino();
        let backups = root.path().join("opt/saltbox/backups");
        let backup = motd_stamped(
            &settings(root.path()),
            &backups,
            "20261115T120000Z",
            &mut None,
        )
        .expect("migrated")
        .expect("a backup");
        assert_eq!(backup, backups.join("motd.yml.20261115T120000Z"));
        assert_eq!(fs::read_to_string(&backup).expect("backup"), T1);
        let saltbox = root.path().join("opt/saltbox");
        for (made, mode, by) in [
            (backup.as_path(), 0o640, owner),
            (backups.as_path(), 0o755, owner),
            (saltbox.as_path(), 0o755, me),
            (root.path().join("opt").as_path(), 0o755, me),
        ] {
            let meta = fs::symlink_metadata(made).expect("meta");
            assert_eq!(meta.mode() & 0o7777, mode, "{}", made.display());
            assert_eq!((meta.uid(), meta.gid()), by, "{}", made.display());
        }
        let meta = fs::metadata(&path).expect("meta");
        assert_ne!(meta.ino(), inode, "written through a new file");
        assert_eq!(meta.mode() & 0o7777, 0o640);
        assert_eq!((meta.uid(), meta.gid()), owner);
        let new = fs::read_to_string(&path).expect("new");
        assert!(
            new.contains("plex:\n  instances:\n    - name: Plex\n"),
            "{new}"
        );
        let mut left: Vec<String> = fs::read_dir(root.path())
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        left.sort();
        assert_eq!(left, ["motd.yml", "opt", "schema"]);
        // Now current: a second run changes nothing.
        assert_eq!(motd(&settings(root.path()), &backups, &mut None), Ok(None));
        assert_eq!(fs::read_dir(&backups).expect("list").count(), 1);
    }

    /// A set-id or sticky bit isn't carried to the backup or the file.
    #[test]
    fn only_the_permission_bits_are_carried() {
        use std::os::unix::fs::MetadataExt as _;
        let root = root(T1);
        let path = root.path().join("motd.yml");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o4640)).expect("chmod");
        let backups = root.path().join("backups");
        let backup = motd_stamped(&settings(root.path()), &backups, "s", &mut None)
            .expect("migrated")
            .expect("a backup");
        for made in [&backup, &path] {
            let meta = fs::metadata(made).expect("meta");
            assert_eq!(meta.mode() & 0o7777, 0o640, "{}", made.display());
        }
    }

    /// No file, a link where the file belongs, a file the plan keeps: no
    /// change and no backup. A backups path that is a file or a link, or a
    /// folder others may write, or a backup already there, fails with the
    /// original unchanged.
    #[test]
    fn nothing_is_written_unless_it_all_succeeds() {
        let root = root(T1);
        let path = root.path().join("motd.yml");
        let backups = root.path().join("backups");
        let settings = settings(root.path());
        fs::remove_file(&path).expect("remove");
        assert_eq!(motd(&settings, &backups, &mut None), Ok(None));
        // An old-format file of the test's own, told apart from motd.yml.
        let elsewhere = root.path().join("elsewhere.yml");
        let other = format!("{T1}# elsewhere\n");
        fs::write(&elsewhere, &other).expect("write");
        std::os::unix::fs::symlink(&elsewhere, &path).expect("link");
        assert_eq!(motd(&settings, &backups, &mut None), Ok(None));
        fs::remove_file(&path).expect("remove");
        fs::write(&path, "plex:\n  - name: Plex\n").expect("write");
        assert_eq!(motd(&settings, &backups, &mut None), Ok(None));
        fs::write(&path, vec![b'#'; MAX_FILE_BYTES + 1]).expect("write");
        assert_eq!(motd(&settings, &backups, &mut None), Ok(None));
        fs::write(&path, b"plex: \xff\n").expect("write");
        assert_eq!(motd(&settings, &backups, &mut None), Ok(None));
        assert!(!backups.exists());
        fs::write(&path, T1).expect("write");
        fs::write(&backups, "").expect("a file in the way");
        let err = motd(&settings, &backups, &mut None).expect_err("not a folder");
        assert!(
            err.ends_with("backups: a link or not a folder, or a link on its path; not followed"),
            "{err}"
        );
        fs::remove_file(&backups).expect("remove");
        let target = root.path().join("target");
        fs::create_dir(&target).expect("dir");
        std::os::unix::fs::symlink(&target, &backups).expect("link");
        let err = motd(&settings, &backups, &mut None).expect_err("a link");
        assert!(
            err.ends_with("backups: a link or not a folder, or a link on its path; not followed"),
            "{err}"
        );
        assert_eq!(fs::read_dir(&target).expect("list").count(), 0);
        fs::remove_file(&backups).expect("remove");
        fs::create_dir(&backups).expect("dir");
        fs::set_permissions(&backups, fs::Permissions::from_mode(0o777)).expect("chmod");
        let err = motd(&settings, &backups, &mut None).expect_err("writable by others");
        assert!(err.contains("writable by others; not used"), "{err}");
        fs::set_permissions(&backups, fs::Permissions::from_mode(0o755)).expect("chmod");
        if as_root(&path) {
            std::os::unix::fs::chown(&backups, Some(4321), None).expect("chown");
            let err = motd(&settings, &backups, &mut None).expect_err("someone else's");
            assert!(
                err.contains("not owned by root or motd.yml's owner"),
                "{err}"
            );
            std::os::unix::fs::chown(&backups, Some(0), None).expect("chown");
        }
        assert_eq!(fs::read_to_string(&path).expect("read"), T1);
        // The backup's name already taken, by a link to a file in the
        // test's folder: refused, the link's target untouched.
        std::os::unix::fs::symlink(&elsewhere, backups.join("motd.yml.taken")).expect("link");
        let err = motd_stamped(&settings, &backups, "taken", &mut None).expect_err("taken");
        assert!(err.contains("File exists"), "{err}");
        assert_eq!(fs::read_to_string(&path).expect("read"), T1);
        assert_eq!(fs::read_to_string(&elsewhere).expect("read"), other);
    }

    /// The Saltbox root's handle, as `motd` opens it.
    fn handle(root: &Path) -> OwnedFd {
        let dir = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        rustix::fs::open(root, dir, Mode::empty()).expect("root")
    }

    /// A file changed after it was read is not replaced, and the
    /// temporary file goes.
    #[test]
    fn a_file_changed_meanwhile_is_not_replaced() {
        let root = root(T1);
        let path = root.path().join("motd.yml");
        let dir = handle(root.path());
        let original = Original::read(&dir).expect("read");
        assert!(original.unchanged(&dir));
        fs::write(&path, format!("{T1}# edited\n")).expect("edit");
        assert!(!original.unchanged(&dir));
        let err = write(&dir, "x: 1\n", &original, "s").expect_err("changed");
        assert_eq!(
            err.to_string(),
            "changed while it was checked; not migrated"
        );
        assert!(
            fs::read_to_string(&path)
                .expect("read")
                .ends_with("# edited\n")
        );
        let mut left: Vec<String> = fs::read_dir(root.path())
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        left.sort();
        assert_eq!(left, ["motd.yml", "schema"]);
    }

    /// A link planted at any path the migration touches is refused, and
    /// nothing is written through it: the Saltbox root, motd.yml, the
    /// temporary file's name, the backups folder and the backup's name.
    /// Every link points at a path in the test's own folder that doesn't
    /// exist, which must still not exist after: these tests run as root,
    /// under cargo-mutants too, whose mutants of the open flags would
    /// otherwise write through a link onto the host.
    #[test]
    fn no_planted_link_is_written_through() {
        let place = tempfile::tempdir().expect("place");
        let outside = place.path().join("outside");
        let link = |at: &Path, name: &str| {
            let target = outside.join(name);
            std::os::unix::fs::symlink(&target, at).expect("link");
            target
        };
        let mut targets = Vec::new();
        // The temporary file's name.
        let saltbox = root(T1);
        let dir = handle(saltbox.path());
        let original = Original::read(&dir).expect("read");
        let temporary = format!(".motd.yml.{}.t.sb-tmp", std::process::id());
        targets.push(link(&saltbox.path().join(&temporary), "temporary"));
        let err = write(&dir, "x: 1\n", &original, "t").expect_err("refused");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(
            fs::read_to_string(saltbox.path().join("motd.yml")).expect("read"),
            T1
        );
        // The backup's name.
        let backups = saltbox.path().join("backups");
        fs::create_dir(&backups).expect("dir");
        targets.push(link(&backups.join("motd.yml.b"), "backup"));
        let err =
            motd_stamped(&settings(saltbox.path()), &backups, "b", &mut None).expect_err("refused");
        assert!(err.contains("File exists"), "{err}");
        // The backups folder.
        let elsewhere = saltbox.path().join("elsewhere");
        targets.push(link(&elsewhere, "folder"));
        let err = motd_stamped(&settings(saltbox.path()), &elsewhere, "c", &mut None)
            .expect_err("refused");
        assert!(
            err.ends_with("a link or not a folder, or a link on its path; not followed"),
            "{err}"
        );
        assert_eq!(
            fs::read_to_string(saltbox.path().join("motd.yml")).expect("read"),
            T1
        );
        // motd.yml itself.
        let other = root(T1);
        fs::remove_file(other.path().join("motd.yml")).expect("remove");
        targets.push(link(&other.path().join("motd.yml"), "motd.yml"));
        assert_eq!(
            motd_stamped(&settings(other.path()), &backups, "d", &mut None),
            Ok(None)
        );
        // The Saltbox root.
        let linked = place.path().join("saltbox");
        targets.push(link(&linked, "saltbox"));
        let mut through_link = settings(saltbox.path());
        through_link.root.clone_from(&linked);
        assert_eq!(
            motd_stamped(&through_link, &backups, "e", &mut None),
            Ok(None)
        );
        // A link above the backups folder, and above the Saltbox root, to
        // a real folder of the test's own: refused, nothing made there.
        let real = place.path().join("real");
        fs::create_dir(&real).expect("dir");
        let up = place.path().join("up");
        std::os::unix::fs::symlink(&real, &up).expect("link");
        let err = motd_stamped(
            &settings(saltbox.path()),
            &up.join("backups"),
            "f",
            &mut None,
        )
        .expect_err("refused");
        assert!(err.contains("a link on its path; not followed"), "{err}");
        let copy = real.join("saltbox");
        fs::create_dir(&copy).expect("dir");
        fs::write(copy.join("motd.yml"), T1).expect("write");
        let mut above = settings(saltbox.path());
        above.root = up.join("saltbox");
        assert_eq!(motd_stamped(&above, &backups, "g", &mut None), Ok(None));
        let mut made: Vec<String> = fs::read_dir(&real)
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        made.sort();
        assert_eq!(made, ["saltbox"]);
        assert_eq!(fs::read_to_string(copy.join("motd.yml")).expect("read"), T1);
        for target in &targets {
            assert!(
                fs::symlink_metadata(target).is_err(),
                "{}",
                target.display()
            );
        }
        assert!(fs::symlink_metadata(&outside).is_err());
    }

    /// A Saltbox root that is a link, or a motd.yml with a second name,
    /// is left alone: nothing is written through them.
    #[test]
    fn a_linked_root_or_a_hard_linked_file_is_left_alone() {
        let target = root(T1);
        let place = tempfile::tempdir().expect("place");
        let linked = place.path().join("saltbox");
        std::os::unix::fs::symlink(target.path(), &linked).expect("link");
        let backups = place.path().join("backups");
        let mut through_link = settings(target.path());
        through_link.root.clone_from(&linked);
        assert_eq!(motd(&through_link, &backups, &mut None), Ok(None));
        assert_eq!(
            fs::read_to_string(target.path().join("motd.yml")).expect("read"),
            T1
        );
        assert!(!backups.exists());
        let root = root(T1);
        fs::hard_link(root.path().join("motd.yml"), root.path().join("other.yml")).expect("link");
        assert_eq!(motd(&settings(root.path()), &backups, &mut None), Ok(None));
        assert_eq!(
            fs::read_to_string(root.path().join("motd.yml")).expect("read"),
            T1
        );
    }
}
