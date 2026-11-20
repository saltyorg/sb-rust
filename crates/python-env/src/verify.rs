//! `sb update --verify-venv` (python-env spec, decision 25): the active
//! venv's files checked against their wheels' `RECORD`s, and its Python
//! release's against the manifest sb wrote when it installed the release.
//!
//! - [`packages`]: every `*.dist-info/RECORD` row with a `sha256=` digest
//!   (urlsafe base64 without padding) and its size, as the binary
//!   distribution format defines them
//!   (<https://packaging.python.org/en/latest/specifications/binary-distribution-format/#the-dist-info-directory>;
//!   RECORD is a CSV file, <https://packaging.python.org/en/latest/specifications/recording-installed-packages/#the-record-file>).
//! - [`write_release_manifest`] and [`check_release`]: the Python
//!   release has no RECORD, so sb hashes its files right after `uv python
//!   install`, while uv's checksum of the archive still vouches for them,
//!   and keeps the manifest under [`Layout::python_manifests`], outside
//!   the Saltbox user's `/srv/python` (step 16 gives that tree to the
//!   user). A manifest the files' owner could have rewritten is never
//!   used.
//!
//! The venv and the release belong to the Saltbox user (step 16), who
//! could put a link or a mount anywhere in them: each file is opened
//! beneath the tree's root with `openat2` and `RESOLVE_NO_SYMLINKS |
//! RESOLVE_NO_XDEV | RESOLVE_BENEATH`, without blocking on a FIFO
//! (`O_NONBLOCK`), as cli-core's `read_beneath` reads a user-writable
//! config (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.openat2.html>;
//! Linux 5.6, below every supported Ubuntu): a link to a device would
//! make root open it. Then it is checked on the open descriptor: a
//! regular file, its size, its sha256 (sb-release's `sha256_reader`).
//! The work is blocking: callers run it under cli-core's
//! `spawn::blocking`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use rustix::fs::{Mode, OFlags, ResolveFlags};
use serde::{Deserialize, Serialize};
use std::os::fd::OwnedFd;
use tokio_util::sync::CancellationToken;

use crate::Layout;

/// The largest RECORD read: a Saltbox venv's largest, ansible's, is
/// about 2 MB; a larger one is bad (a sparse file could grow csv's
/// field buffer without end).
const RECORD_CAP: u64 = 32 << 20;

/// How many threads check files at once: the 2026-11-19 measurement's
/// four (0.3 s warm over a Saltbox venv's 30,538 files, one thread 1 s).
pub const THREADS: usize = 4;

/// What a check found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Findings {
    /// Files read and compared.
    pub checked: usize,
    /// Files whose digest or size differs, or that aren't a regular file
    /// (a link, a directory, a FIFO), or a list that can't be read.
    pub bad: Vec<PathBuf>,
    /// Files that aren't there.
    pub missing: Vec<PathBuf>,
    /// Files two RECORDs list with different digests: not checked, and
    /// no damage (a rebuild wouldn't change them).
    pub conflicting: Vec<PathBuf>,
}

impl Findings {
    /// Whether anything is bad or missing.
    #[must_use]
    pub fn damaged(&self) -> bool {
        !self.bad.is_empty() || !self.missing.is_empty()
    }

    fn merge(&mut self, other: Self) {
        self.checked += other.checked;
        self.bad.extend(other.bad);
        self.missing.extend(other.missing);
        self.conflicting.extend(other.conflicting);
    }

    fn sort(mut self) -> Self {
        self.bad.sort();
        self.missing.sort();
        self
    }
}

/// The check was stopped by Ctrl-C (the token).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("interrupted while verifying the Ansible virtual environment's files")]
pub struct Interrupted;

/// One file and what it should hold.
#[derive(Debug)]
struct Expected {
    /// Its path, as reported.
    path: PathBuf,
    /// The same beneath the tree's root, as opened.
    relative: PathBuf,
    sha256: [u8; 32],
    size: Option<u64>,
}

/// The active venv's files against their RECORDs: each
/// `lib/python*/site-packages/*.dist-info/RECORD`, every row with a
/// `sha256=` digest. A row naming a path outside `venv` counts as bad, as
/// does a RECORD that can't be read or a digest that isn't one; a venv
/// with no `site-packages` counts it as missing.
///
/// # Errors
///
/// [`Interrupted`] when `token` is cancelled.
pub fn packages(venv: &Path, token: &CancellationToken) -> Result<Findings, Interrupted> {
    let mut findings = Findings::default();
    let sites = site_packages(venv);
    let root = match open_root(venv) {
        Ok(root) if !sites.is_empty() => root,
        _ => {
            findings.missing.push(venv.join("lib"));
            return Ok(findings);
        }
    };
    let mut expected = Vec::new();
    for site in &sites {
        for record in records(site) {
            read_record(&root, venv, site, &record, &mut expected, &mut findings);
        }
    }
    let expected = distinct(expected, &mut findings);
    findings.merge(check_all(&root, &expected, token)?);
    Ok(findings.sort())
}

/// `dir` as a descriptor files are opened beneath (`O_PATH`), itself not
/// a link.
fn open_root(dir: &Path) -> io::Result<OwnedFd> {
    Ok(rustix::fs::open(dir, ROOT, Mode::empty())?)
}

/// Each file once: rows naming the same path with the same digest and
/// size are one; rows that disagree (two wheels shipping one file, the
/// later one's bytes on disk) go to [`Findings::conflicting`], which a
/// rebuild couldn't change, and aren't checked.
fn distinct(expected: Vec<Expected>, findings: &mut Findings) -> Vec<Expected> {
    let mut by_path: BTreeMap<PathBuf, Expected> = BTreeMap::new();
    let mut conflicting = std::collections::BTreeSet::new();
    for file in expected {
        match by_path.get(&file.path) {
            Some(seen) if seen.sha256 != file.sha256 || seen.size != file.size => {
                conflicting.insert(file.path);
            }
            Some(_) => {}
            None => {
                by_path.insert(file.path.clone(), file);
            }
        }
    }
    for path in &conflicting {
        by_path.remove(path);
    }
    findings.conflicting.extend(conflicting);
    by_path.into_values().collect()
}

/// `venv/lib/python*/site-packages`, the ones that are directories.
fn site_packages(venv: &Path) -> Vec<PathBuf> {
    let real_dir = |path: &Path| path.symlink_metadata().is_ok_and(|meta| meta.is_dir());
    let lib = venv.join("lib");
    // A link on the way is never listed (a FUSE mount behind one could
    // hang the listing); the files' opens refuse it anyway.
    let entries = match std::fs::read_dir(&lib) {
        Ok(entries) if real_dir(&lib) => entries,
        _ => return Vec::new(),
    };
    let mut sites: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("python"))
        .filter(|entry| real_dir(&entry.path()))
        .map(|entry| entry.path().join("site-packages"))
        .filter(|site| real_dir(site))
        .collect();
    sites.sort();
    sites
}

/// Every `*.dist-info/RECORD` in `site`.
fn records(site: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(site) else {
        return Vec::new();
    };
    let mut records: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".dist-info"))
        .map(|entry| entry.path().join("RECORD"))
        .collect();
    records.sort();
    records
}

/// `record`'s rows with a sha256 into `expected`; what's wrong with it
/// into `findings`.
fn read_record(
    root: &OwnedFd,
    venv: &Path,
    site: &Path,
    record: &Path,
    expected: &mut Vec<Expected>,
    findings: &mut Findings,
) {
    let relative = record.strip_prefix(venv).unwrap_or(record);
    let file = match open_beneath(root, relative) {
        Ok(Some((file, meta))) if meta.len() <= RECORD_CAP => file,
        Ok(None) => {
            findings.missing.push(record.to_path_buf());
            return;
        }
        // Past the cap, or not a regular file beneath the venv.
        Ok(Some(_)) | Err(_) => {
            findings.bad.push(record.to_path_buf());
            return;
        }
    };
    // RECORD has no header row; a row may have fewer fields (RECORD's
    // own has no digest)
    // (<https://docs.rs/csv/1.4.0/csv/struct.ReaderBuilder.html#method.flexible>).
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(io::Read::take(file, RECORD_CAP));
    for row in reader.records() {
        let Ok(row) = row else {
            findings.bad.push(record.to_path_buf());
            return;
        };
        let (Some(name), Some(hash)) = (row.get(0), row.get(1)) else {
            continue;
        };
        // Only sha256 is checked: what uv and pip write. A row without a
        // digest (RECORD itself, `.pyc` files) has nothing to check.
        let Some(encoded) = hash.strip_prefix("sha256=") else {
            continue;
        };
        let Some(path) = within(venv, &site.join(name)) else {
            findings.bad.push(site.join(name));
            continue;
        };
        // <https://docs.rs/base64/0.22.1/base64/engine/general_purpose/constant.URL_SAFE_NO_PAD.html>
        let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()
            .and_then(|digest| <[u8; 32]>::try_from(digest).ok());
        let Some(sha256) = digest else {
            findings.bad.push(path);
            continue;
        };
        // uv and pip always write the size: a hashed row without one is
        // bad, so no file is read to its end unbounded.
        let Some(length) = row.get(2).and_then(|field| field.parse().ok()) else {
            findings.bad.push(path);
            continue;
        };
        let relative = path.strip_prefix(venv).unwrap_or(&path).to_path_buf();
        expected.push(Expected {
            path,
            relative,
            sha256,
            size: Some(length),
        });
    }
}

/// `path` with `.` and `..` resolved lexically, when it stays inside
/// `root` (a RECORD names scripts as `../../../bin/<name>`); `None`
/// otherwise. Why not std: `Path::normalize_lexically` is unstable
/// (<https://doc.rust-lang.org/std/path/struct.Path.html#method.normalize_lexically>),
/// and `canonicalize` follows links, which a check must not.
fn within(root: &Path, path: &Path) -> Option<PathBuf> {
    let mut resolved = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    (resolved.starts_with(root) && resolved != root).then_some(resolved)
}

/// A file opened to read: never waiting on a FIFO, never a controlling
/// terminal. (Constants, as the flag sets below: bitflags' `union`.)
const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NONBLOCK)
    .union(OFlags::NOCTTY)
    .union(OFlags::CLOEXEC);

/// A tree's root (or a link's directory) held for `*at` calls, itself
/// never a link.
const ROOT: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The manifests' directory, opened to `fstat` and `openat` in it.
const MANIFESTS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// How a path beneath a tree's root is resolved: no link (magic links
/// included) or mount point on the way, never above the root.
const BENEATH: ResolveFlags = ResolveFlags::NO_SYMLINKS
    .union(ResolveFlags::NO_MAGICLINKS)
    .union(ResolveFlags::NO_XDEV)
    .union(ResolveFlags::BENEATH);

/// `relative` beneath `root`, opened for reading if it is a regular file:
/// no link or mount point anywhere on the way, never above `root`
/// (`openat2`, see the module docs); `None` when it isn't there. A link
/// or mount on the way (`ELOOP`, `EXDEV`) is an error, as is anything
/// but a regular file.
fn open_beneath(root: &OwnedFd, relative: &Path) -> io::Result<Option<(File, std::fs::Metadata)>> {
    match rustix::fs::openat2(root, relative, READ, Mode::empty(), BENEATH) {
        Ok(fd) => regular(File::from(fd)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(errno) => Err(errno.into()),
    }
}

/// `file` with its `fstat`, when it is a regular file.
fn regular(file: File) -> io::Result<Option<(File, std::fs::Metadata)>> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(Some((file, meta)))
}

/// Check one file: missing, bad (not a regular file, another size or
/// digest, unreadable) or good.
fn check(root: &OwnedFd, expected: &Expected, token: &CancellationToken, findings: &mut Findings) {
    let (file, meta) = match open_beneath(root, &expected.relative) {
        Ok(Some(opened)) => opened,
        Ok(None) => {
            findings.missing.push(expected.path.clone());
            return;
        }
        Err(_) => {
            findings.bad.push(expected.path.clone());
            return;
        }
    };
    findings.checked += 1;
    if expected.size.is_some_and(|size| size != meta.len()) {
        findings.bad.push(expected.path.clone());
        return;
    }
    match sb_release::sha256_reader(Cancellable { file, token }) {
        Ok(digest) if digest == expected.sha256 => {}
        _ => findings.bad.push(expected.path.clone()),
    }
}

/// A file read that ends when `token` is cancelled, so Ctrl-C stops a
/// long read at its next 64 KiB part, not at the file's end.
struct Cancellable<'a> {
    file: File,
    token: &'a CancellationToken,
}

impl io::Read for Cancellable<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.token.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        self.file.read(buf)
    }
}

/// Every file of `expected` checked on [`THREADS`] threads (cli-core's
/// `scoped_thread`, which carries the caller's span), each taking the
/// next file until none is left or `token` is cancelled: std's scoped
/// threads (<https://doc.rust-lang.org/std/thread/fn.scope.html>) and an
/// atomic index. Why not rayon: it isn't in the tree, and four workers
/// over one list need no pool.
fn check_all(
    root: &OwnedFd,
    expected: &[Expected],
    token: &CancellationToken,
) -> Result<Findings, Interrupted> {
    let next = AtomicUsize::new(0);
    let found = Mutex::new(Findings::default());
    let work = || {
        let mut findings = Findings::default();
        while !token.is_cancelled() {
            let Some(file) = expected.get(next.fetch_add(1, Ordering::Relaxed)) else {
                break;
            };
            check(root, file, token, &mut findings);
        }
        found
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .merge(findings);
    };
    std::thread::scope(|scope| {
        let workers: Vec<_> = (1..THREADS)
            .filter_map(|_| {
                sb_cli_core::spawn::scoped_thread(std::thread::Builder::new(), scope, work).ok()
            })
            .collect();
        // This thread works too, so a failed spawn only slows the check.
        work();
        for worker in workers {
            // A worker's panic is this one's (M-PANIC-IS-STOP): its files
            // must not pass as checked.
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
        }
    });
    if token.is_cancelled() {
        return Err(Interrupted);
    }
    Ok(found
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// The manifest's format, version 1.
const SCHEMA: u32 = 1;

/// A Python release's files as sb found them after installing it: each
/// regular file's sha256 (lower-case hex) and size, each link's target,
/// by path relative to the release. `__pycache__` and `.pyc` files are
/// left out: Python rewrites them itself.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ReleaseManifest {
    schema: u32,
    files: BTreeMap<String, FileEntry>,
    links: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct FileEntry {
    sha256: String,
    size: u64,
}

/// Where `release`'s manifest lives: `<python_manifests>/<release's
/// name>.json`.
#[must_use]
pub fn manifest_path(layout: &Layout, release: &Path) -> PathBuf {
    let name = release.file_name().unwrap_or_default().to_string_lossy();
    layout.python_manifests.join(format!("{name}.json"))
}

/// Why a release's manifest couldn't be written.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("walk Python release {}: {cause}", release.display())]
    Walk { release: PathBuf, cause: io::Error },
    #[error("hash {}: {cause}", path.display())]
    Hash { path: PathBuf, cause: io::Error },
    #[error("write Python release manifest {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
}

/// Hash every file of `release`, which must be `owner`'s (sb's euid), into
/// its manifest at `manifest`, mode
/// 0644, written through a temporary file and renamed into place
/// (tempfile's `persist`), its directory made 0755 when missing: the
/// number of files hashed.
///
/// # Errors
///
/// [`WriteError`]: the walk, a file's hash or the write.
pub fn write_release_manifest(
    release: &Path,
    manifest: &Path,
    owner: u32,
) -> Result<usize, WriteError> {
    let walked = |cause: io::Error| WriteError::Walk {
        release: release.to_path_buf(),
        cause,
    };
    // `/srv/python/releases` is the Saltbox user's (step 16), who could
    // swap the release for a tree of their own after uv's install: the
    // release is held open, must be `owner`'s (sb made it), and every
    // file is read beneath that descriptor, whatever the path names now.
    let root = open_root(release).map_err(walked)?;
    let held = rustix::fs::fstat(&root).map_err(|errno| walked(errno.into()))?;
    if held.st_uid != owner {
        return Err(walked(io::Error::other(format!(
            "owned by uid {}, not {owner}",
            held.st_uid
        ))));
    }
    let mut files = BTreeMap::new();
    let mut links = BTreeMap::new();
    let walk = walkdir::WalkDir::new(release)
        .follow_links(false)
        .follow_root_links(false)
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "__pycache__");
    for entry in walk {
        let entry = entry.map_err(|err| WriteError::Walk {
            release: release.to_path_buf(),
            cause: err.into(),
        })?;
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(release) else {
            continue;
        };
        let name = relative.to_string_lossy().into_owned();
        let kind = entry.file_type();
        let hash_failed = |cause| WriteError::Hash {
            path: path.to_path_buf(),
            cause,
        };
        if kind.is_symlink() {
            let target = link_beneath(&root, relative)
                .and_then(|found| found.ok_or_else(|| io::ErrorKind::NotFound.into()))
                .map_err(hash_failed)?;
            links.insert(name, target.to_string_lossy().into_owned());
        } else if kind.is_file() && path.extension().is_none_or(|ext| ext != "pyc") {
            let hashed = open_beneath(&root, relative)
                .and_then(|opened| opened.ok_or_else(|| io::ErrorKind::NotFound.into()))
                .and_then(|(file, meta)| Ok((sb_release::sha256_reader(file)?, meta.len())));
            let (digest, size) = hashed.map_err(hash_failed)?;
            let sha256 = hex::encode(digest);
            files.insert(name, FileEntry { sha256, size });
        }
    }
    let count = files.len();
    let text = serde_json::to_vec(&ReleaseManifest {
        schema: SCHEMA,
        files,
        links,
    })
    .map_err(io::Error::from);
    let failed = |cause| WriteError::Write {
        path: manifest.to_path_buf(),
        cause,
    };
    let text = text.map_err(failed)?;
    let dir = manifest.parent().unwrap_or(manifest);
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o755)
        .create(dir)
        .map_err(failed)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".manifest-")
        .tempfile_in(dir)
        .map_err(failed)?;
    io::Write::write_all(&mut temporary, &text).map_err(failed)?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))
        .map_err(failed)?;
    temporary.as_file().sync_all().map_err(failed)?;
    temporary
        .persist(manifest)
        .map_err(|err| failed(err.error))?;
    Ok(count)
}

/// What [`check_release`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseCheck {
    /// The release's files against its manifest.
    Checked(Findings),
    /// No manifest yet: a release installed before sb wrote them, or by
    /// sb-go. Its next install writes one.
    NoManifest,
    /// A manifest there, but not one sb can trust: why.
    Untrusted(String),
}

/// `release`'s files against its manifest at `manifest`, trusted only
/// when it and its directory are owned by `owner` (sb's euid, root on a
/// host), aren't writable by group or others, and the manifest is a
/// regular file (opened without following a link). A file the manifest
/// lists that is missing, isn't a regular file (a link where a file was,
/// or a file where a link was), or differs in size, digest or a link's
/// target, is reported.
///
/// # Errors
///
/// [`Interrupted`] when `token` is cancelled.
pub fn check_release(
    release: &Path,
    manifest: &Path,
    owner: u32,
    token: &CancellationToken,
) -> Result<ReleaseCheck, Interrupted> {
    let stored = match trusted(manifest, owner) {
        Trust::Missing => return Ok(ReleaseCheck::NoManifest),
        Trust::Refused(why) => return Ok(ReleaseCheck::Untrusted(why)),
        Trust::Read(stored) => stored,
    };
    let mut findings = Findings::default();
    let Ok(root) = open_root(release) else {
        findings.missing.push(release.to_path_buf());
        return Ok(ReleaseCheck::Checked(findings));
    };
    let mut expected = Vec::new();
    for (name, entry) in &stored.files {
        let Some(path) = within(release, &release.join(name)) else {
            findings.bad.push(release.join(name));
            continue;
        };
        let relative = path.strip_prefix(release).unwrap_or(&path).to_path_buf();
        let digest = hex::decode(&entry.sha256)
            .ok()
            .and_then(|digest| <[u8; 32]>::try_from(digest).ok());
        let Some(sha256) = digest else {
            findings.bad.push(path);
            continue;
        };
        expected.push(Expected {
            path,
            relative,
            sha256,
            size: Some(entry.size),
        });
    }
    for (name, target) in &stored.links {
        let Some(path) = within(release, &release.join(name)) else {
            findings.bad.push(release.join(name));
            continue;
        };
        let relative = path.strip_prefix(release).unwrap_or(&path);
        match link_beneath(&root, relative) {
            Ok(None) => findings.missing.push(path),
            Ok(Some(found)) if found == Path::new(target) => findings.checked += 1,
            Ok(Some(_)) => {
                findings.checked += 1;
                findings.bad.push(path);
            }
            Err(_) => findings.bad.push(path),
        }
    }
    findings.merge(check_all(&root, &expected, token)?);
    Ok(ReleaseCheck::Checked(findings.sort()))
}

/// The target of the link `relative` beneath `root`: its directory opened
/// as [`open_beneath`] opens a file (`O_PATH`), then `readlinkat` on its
/// last part, which reads the link without following it; `None` when it
/// isn't there.
fn link_beneath(root: &OwnedFd, relative: &Path) -> io::Result<Option<PathBuf>> {
    let (Some(parent), Some(name)) = (relative.parent(), relative.file_name()) else {
        return Err(io::Error::other("no link name"));
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let dir = match rustix::fs::openat2(root, parent, ROOT, Mode::empty(), BENEATH) {
        Ok(dir) => dir,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(errno) => return Err(errno.into()),
    };
    match rustix::fs::readlinkat(&dir, name, Vec::new()) {
        Ok(target) => {
            let target: std::ffi::OsString =
                std::os::unix::ffi::OsStringExt::from_vec(target.into_bytes());
            Ok(Some(PathBuf::from(target)))
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(errno) => Err(errno.into()),
    }
}

/// A manifest as [`trusted`] found it.
enum Trust {
    Missing,
    Refused(String),
    Read(ReleaseManifest),
}

/// The manifest at `path`, if it and its directory are `owner`'s and
/// written by nobody else.
fn trusted(path: &Path, owner: u32) -> Trust {
    let dir = path.parent().unwrap_or(path);
    // The directory held open (not a link) and checked on its
    // descriptor, the manifest opened beneath that same descriptor: what
    // is checked is what is read.
    let dir_fd = match rustix::fs::open(dir, MANIFESTS, Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Trust::Missing,
        Err(errno) => {
            return Trust::Refused(format!("{}: {}", dir.display(), io::Error::from(errno)));
        }
    };
    let held = match rustix::fs::fstat(&dir_fd) {
        Ok(held) => held,
        Err(errno) => {
            return Trust::Refused(format!("{}: {}", dir.display(), io::Error::from(errno)));
        }
    };
    let is_dir =
        rustix::fs::FileType::from_raw_mode(held.st_mode) == rustix::fs::FileType::Directory;
    if let Some(why) = refused(dir, is_dir, held.st_uid, held.st_mode, owner, true) {
        return Trust::Refused(why);
    }
    let Some(name) = path.file_name() else {
        return Trust::Missing;
    };
    let opened =
        match rustix::fs::openat(&dir_fd, name, READ.union(OFlags::NOFOLLOW), Mode::empty()) {
            Ok(fd) => regular(File::from(fd)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(errno) => Err(io::Error::from(errno)),
        };
    let (file, meta) = match opened {
        Ok(Some(opened)) => opened,
        Ok(None) => return Trust::Missing,
        Err(err) => return Trust::Refused(format!("{}: {err}", path.display())),
    };
    if let Some(why) = refused(path, meta.is_file(), meta.uid(), meta.mode(), owner, false) {
        return Trust::Refused(why);
    }
    match serde_json::from_reader::<_, ReleaseManifest>(io::BufReader::new(file)) {
        Ok(stored) if stored.schema == SCHEMA => Trust::Read(stored),
        Ok(stored) => Trust::Refused(format!(
            "{}: schema {}, not {SCHEMA}",
            path.display(),
            stored.schema
        )),
        Err(err) => Trust::Refused(format!("{}: {err}", path.display())),
    }
}

/// Why `path` (its kind as expected or not, its owner and mode, from its
/// open descriptor's `fstat`) can't be trusted, if it can't: not the
/// kind expected, another owner, or writable by group or others.
fn refused(
    path: &Path,
    kind_ok: bool,
    uid: u32,
    mode: u32,
    owner: u32,
    dir: bool,
) -> Option<String> {
    if !kind_ok {
        let want = if dir { "a directory" } else { "a regular file" };
        return Some(format!("{} is not {want}", path.display()));
    }
    if uid != owner {
        return Some(format!(
            "{} is owned by uid {uid}, not {owner}",
            path.display()
        ));
    }
    if mode & 0o022 != 0 {
        return Some(format!(
            "{} is writable by group or others (mode {:o})",
            path.display(),
            mode & 0o7777
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    /// RECORD's digest of `bytes`: sha256, urlsafe base64, no padding.
    fn digest(bytes: &[u8]) -> String {
        let sha = sb_release::sha256_reader(bytes).expect("digest");
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha)
    }

    /// A venv with one wheel `pkg` whose RECORD lists a module, a
    /// script in `bin/` (as `../../../bin/tool`), a name with a comma
    /// (quoted, as Python's csv writes it), RECORD itself without a
    /// digest and a `.pyc` without one.
    fn venv() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let venv = dir.path().join("venv");
        let site = venv.join("lib/python3.12/site-packages");
        std::fs::create_dir_all(site.join("pkg/__pycache__")).expect("pkg");
        std::fs::create_dir_all(site.join("pkg-1.0.dist-info")).expect("dist-info");
        std::fs::create_dir_all(venv.join("bin")).expect("bin");
        let files: [(&str, PathBuf, &[u8]); 3] = [
            (
                "pkg/__init__.py",
                site.join("pkg/__init__.py"),
                b"VALUE = 1\n",
            ),
            ("../../../bin/tool", venv.join("bin/tool"), b"#!/bin/sh\n"),
            ("\"pkg/a,b.py\"", site.join("pkg/a,b.py"), b"x = 2\n"),
        ];
        let mut record = String::new();
        for (name, path, bytes) in files {
            std::fs::write(&path, bytes).expect("file");
            let row = format!("{name},sha256={},{}\n", digest(bytes), bytes.len());
            record.push_str(&row);
        }
        std::fs::write(site.join("pkg/__pycache__/x.pyc"), b"pyc").expect("pyc");
        record.push_str("pkg/__pycache__/x.pyc,,\npkg-1.0.dist-info/RECORD,,\n");
        std::fs::write(site.join("pkg-1.0.dist-info/RECORD"), record).expect("record");
        (dir, venv, site)
    }

    fn token() -> CancellationToken {
        CancellationToken::new()
    }

    #[test]
    fn a_clean_venv_has_every_digest_checked() {
        let (_dir, venv, _) = venv();
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(
            found,
            Findings {
                checked: 3,
                ..Findings::default()
            }
        );
        assert!(!found.damaged());
    }

    #[test]
    fn a_poisoned_a_resized_and_a_missing_file_are_found() {
        let (_dir, venv, site) = venv();
        // The same size, one byte changed.
        std::fs::write(site.join("pkg/__init__.py"), b"VALUE = 2\n").expect("poison");
        // A size change only shows in the size.
        std::fs::write(venv.join("bin/tool"), b"#!/bin/sh\n\n").expect("grow");
        std::fs::remove_file(site.join("pkg/a,b.py")).expect("rm");
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(found.checked, 2);
        assert_eq!(
            found.bad,
            [venv.join("bin/tool"), site.join("pkg/__init__.py")]
        );
        assert_eq!(found.missing, [site.join("pkg/a,b.py")]);
        assert!(found.damaged());
    }

    #[test]
    fn a_link_or_a_fifo_where_a_file_was_is_bad_and_never_followed() {
        let (dir, venv, site) = venv();
        let outside = dir.path().join("outside.py");
        std::fs::write(&outside, b"VALUE = 1\n").expect("outside");
        std::fs::remove_file(site.join("pkg/__init__.py")).expect("rm");
        symlink(&outside, site.join("pkg/__init__.py")).expect("link");
        std::fs::remove_file(venv.join("bin/tool")).expect("rm");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            venv.join("bin/tool"),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o644),
            0,
        )
        .expect("fifo");
        // A FIFO with no writer: opened without blocking, refused.
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(
            found.bad,
            [venv.join("bin/tool"), site.join("pkg/__init__.py")]
        );
    }

    #[test]
    fn a_row_outside_the_venv_or_with_a_broken_digest_is_bad() {
        let (dir, venv, site) = venv();
        let record = site.join("pkg-1.0.dist-info/RECORD");
        let mut text = std::fs::read_to_string(&record).expect("record");
        text.push_str("../../../../escape.py,sha256=AAAA,1\npkg/odd.py,sha256=!!,1\n");
        std::fs::write(&record, text).expect("record");
        std::fs::write(dir.path().join("escape.py"), "x").expect("escape");
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(found.checked, 3);
        assert_eq!(
            found.bad,
            // The row's own path, unresolved: what RECORD said.
            [site.join("../../../../escape.py"), site.join("pkg/odd.py")]
        );
    }

    #[test]
    fn a_venv_without_site_packages_or_a_record_is_damaged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let found = packages(dir.path(), &token()).expect("checked");
        assert_eq!(found.missing, [dir.path().join("lib")]);
        let (_dir, venv, site) = venv();
        std::fs::remove_file(site.join("pkg-1.0.dist-info/RECORD")).expect("rm");
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(found.missing, [site.join("pkg-1.0.dist-info/RECORD")]);
    }

    /// A directory link on the way to a listed file is refused, whatever
    /// it points at: root never opens through it (`RESOLVE_NO_SYMLINKS`).
    #[test]
    fn a_link_on_the_way_to_a_file_is_bad_and_never_followed() {
        let (dir, venv, site) = venv();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::write(outside.join("mod.py"), b"VALUE = 1\n").expect("module");
        symlink(&outside, site.join("evil")).expect("link");
        let record = site.join("pkg-1.0.dist-info/RECORD");
        let mut text = std::fs::read_to_string(&record).expect("record");
        let row = format!("evil/mod.py,sha256={},10\n", digest(b"VALUE = 1\n"));
        text.push_str(&row);
        std::fs::write(&record, text).expect("record");
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(found.checked, 3);
        assert_eq!(found.bad, [site.join("evil/mod.py")]);
    }

    /// A hashed row without a size, and a RECORD past the cap, are bad:
    /// neither is read without bound.
    #[test]
    fn a_row_without_a_size_or_an_oversized_record_is_bad() {
        let (_dir, venv, site) = venv();
        let record = site.join("pkg-1.0.dist-info/RECORD");
        let mut text = std::fs::read_to_string(&record).expect("record");
        let row = format!("pkg/__init__.py,sha256={},\n", digest(b"VALUE = 1\n"));
        text.push_str(&row);
        std::fs::write(&record, &text).expect("record");
        let found = packages(&venv, &token()).expect("checked");
        // The same path with no size: bad; the sized row still checks.
        assert_eq!(found.bad, [site.join("pkg/__init__.py")]);
        std::fs::File::options()
            .write(true)
            .open(&record)
            .expect("record")
            .set_len(RECORD_CAP + 1)
            .expect("sparse");
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(found.bad, [record]);
        assert_eq!(found.checked, 0);
    }

    /// Two RECORDs listing one file with different digests (the later
    /// wheel's bytes on disk): not checked, not damage; agreeing rows are
    /// one file.
    #[test]
    fn rows_that_disagree_are_set_apart_and_agreeing_ones_counted_once() {
        let (_dir, venv, site) = venv();
        std::fs::create_dir(site.join("other-2.0.dist-info")).expect("dist-info");
        let rows = format!(
            "pkg/__init__.py,sha256={},10\npkg/a,b.py,sha256={},6\n",
            digest(b"VALUE = 9\n"),
            digest(b"x = 2\n")
        )
        .replace("pkg/a,b.py", "\"pkg/a,b.py\"");
        std::fs::write(site.join("other-2.0.dist-info/RECORD"), rows).expect("record");
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(found.conflicting, [site.join("pkg/__init__.py")]);
        assert_eq!(found.checked, 2);
        assert!(!found.damaged());
    }

    /// A venv whose `lib` is a link isn't listed through it: its
    /// packages count as missing.
    #[test]
    fn a_linked_lib_is_not_followed() {
        let (dir, venv, _) = venv();
        let moved = dir.path().join("lib-elsewhere");
        std::fs::rename(venv.join("lib"), &moved).expect("move");
        symlink(&moved, venv.join("lib")).expect("link");
        let found = packages(&venv, &token()).expect("checked");
        assert_eq!(found.missing, [venv.join("lib")]);
        assert_eq!(found.checked, 0);
    }

    #[test]
    fn a_cancelled_check_is_interrupted() {
        let (_dir, venv, _) = venv();
        let token = token();
        token.cancel();
        assert_eq!(packages(&venv, &token), Err(Interrupted));
    }

    /// A Python release: a file, a link, a `__pycache__` and a `.pyc`.
    fn release() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let release = dir.path().join("releases/python-3-12-15-x");
        let lib = release.join("cpython/lib/python3.12");
        std::fs::create_dir_all(lib.join("__pycache__")).expect("lib");
        std::fs::create_dir_all(release.join("cpython/bin")).expect("bin");
        std::fs::write(lib.join("smtplib.py"), "import socket\n").expect("smtplib");
        std::fs::write(release.join("cpython/bin/python3.12"), "elf").expect("python");
        symlink("python3.12", release.join("cpython/bin/python3")).expect("link");
        std::fs::write(lib.join("__pycache__/smtplib.pyc"), "pyc").expect("pyc");
        std::fs::write(lib.join("loose.pyc"), "pyc").expect("pyc");
        let manifest = dir.path().join("manifests/python-3-12-15-x.json");
        (dir, release, manifest)
    }

    fn me() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    #[test]
    fn a_release_manifest_is_written_0644_and_checks_clean() {
        let (_dir, release, manifest) = release();
        assert_eq!(
            write_release_manifest(&release, &manifest, me()).expect("written"),
            2
        );
        let mode =
            |path: &Path| std::fs::metadata(path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&manifest), 0o644);
        assert_eq!(mode(manifest.parent().expect("dir")), 0o755);
        // Python's own bytecode may change: it isn't listed.
        std::fs::write(release.join("cpython/lib/python3.12/loose.pyc"), "new").expect("pyc");
        let checked = check_release(&release, &manifest, me(), &token()).expect("checked");
        assert_eq!(
            checked,
            ReleaseCheck::Checked(Findings {
                checked: 3,
                ..Findings::default()
            })
        );
    }

    #[test]
    fn a_changed_stdlib_file_a_missing_one_and_a_moved_link_are_found() {
        let (_dir, release, manifest) = release();
        write_release_manifest(&release, &manifest, me()).expect("written");
        let lib = release.join("cpython/lib/python3.12");
        std::fs::write(lib.join("smtplib.py"), "import sockex\n").expect("poison");
        std::fs::remove_file(release.join("cpython/bin/python3.12")).expect("rm");
        std::fs::remove_file(release.join("cpython/bin/python3")).expect("rm");
        symlink("/bin/sh", release.join("cpython/bin/python3")).expect("link");
        let ReleaseCheck::Checked(found) =
            check_release(&release, &manifest, me(), &token()).expect("checked")
        else {
            panic!("not checked");
        };
        assert_eq!(
            found.bad,
            [release.join("cpython/bin/python3"), lib.join("smtplib.py")]
        );
        assert_eq!(found.missing, [release.join("cpython/bin/python3.12")]);
        // The changed file and the moved link were read; the missing file
        // wasn't.
        assert_eq!(found.checked, 2);
    }

    /// A release removed after its manifest was written is one missing
    /// path, the release itself; nothing beneath it is opened.
    #[test]
    fn a_release_gone_since_its_manifest_is_missing() {
        let (_dir, release, manifest) = release();
        write_release_manifest(&release, &manifest, me()).expect("written");
        std::fs::remove_dir_all(&release).expect("rm");
        assert_eq!(
            check_release(&release, &manifest, me(), &token()),
            Ok(ReleaseCheck::Checked(Findings {
                missing: vec![release.clone()],
                ..Findings::default()
            }))
        );
    }

    /// A file where the manifest has a link is bad, and a link that is
    /// gone is missing: the link's kind is checked, not only its target.
    #[test]
    fn a_link_turned_into_a_file_is_bad_and_a_removed_one_missing() {
        let (_dir, release, manifest) = release();
        let bin = release.join("cpython/bin");
        symlink("python3.12", bin.join("python")).expect("link");
        write_release_manifest(&release, &manifest, me()).expect("written");
        std::fs::remove_file(bin.join("python3")).expect("rm");
        std::fs::write(bin.join("python3"), "elf").expect("file");
        std::fs::remove_file(bin.join("python")).expect("rm");
        let ReleaseCheck::Checked(found) =
            check_release(&release, &manifest, me(), &token()).expect("checked")
        else {
            panic!("not checked");
        };
        assert_eq!(found.bad, [bin.join("python3")]);
        assert_eq!(found.missing, [bin.join("python")]);
        // The two regular files; neither changed link was read.
        assert_eq!(found.checked, 2);
    }

    /// A manifest entry naming a path above the release, or with a digest
    /// that isn't one, is bad and never opened: a damaged manifest can't
    /// make root read outside the release.
    #[test]
    fn a_manifest_entry_outside_the_release_or_with_a_broken_digest_is_bad() {
        let (dir, release, manifest) = release();
        write_release_manifest(&release, &manifest, me()).expect("written");
        // The same bytes as the entry names: read, it would count as good.
        std::fs::write(dir.path().join("escape"), "elf").expect("escape");
        let mut stored: ReleaseManifest =
            serde_json::from_slice(&std::fs::read(&manifest).expect("read")).expect("parse");
        let digest = stored.files["cpython/bin/python3.12"].sha256.clone();
        stored.files.insert(
            "../../escape".to_owned(),
            FileEntry {
                sha256: digest,
                size: 3,
            },
        );
        stored
            .files
            .get_mut("cpython/lib/python3.12/smtplib.py")
            .expect("smtplib")
            .sha256 = "not hex".to_owned();
        stored
            .links
            .insert("../../escape-link".to_owned(), "python3.12".to_owned());
        std::fs::write(&manifest, serde_json::to_vec(&stored).expect("json")).expect("write");
        let ReleaseCheck::Checked(found) =
            check_release(&release, &manifest, me(), &token()).expect("checked")
        else {
            panic!("not checked");
        };
        assert_eq!(
            found.bad,
            [
                release.join("../../escape"),
                release.join("../../escape-link"),
                release.join("cpython/lib/python3.12/smtplib.py"),
            ]
        );
        // The untouched file and link only.
        assert_eq!(found.checked, 2);
        assert_eq!(found.missing, Vec::<PathBuf>::new());
    }

    /// A manifest whose directory is a file, or that is a directory
    /// itself, is refused with its path, not read.
    #[test]
    fn a_manifest_that_isnt_a_file_in_a_directory_is_refused() {
        let (dir, release, _) = release();
        let file_dir = dir.path().join("manifests");
        std::fs::write(&file_dir, "not a directory").expect("file");
        let manifest = file_dir.join("python-3-12-15-x.json");
        match check_release(&release, &manifest, me(), &token()) {
            Ok(ReleaseCheck::Untrusted(why)) => assert_eq!(
                why,
                format!(
                    "{}: {}",
                    file_dir.display(),
                    io::Error::from(rustix::io::Errno::NOTDIR)
                )
            ),
            other => panic!("used: {other:?}"),
        }
        std::fs::remove_file(&file_dir).expect("rm");
        // Both 0755 whatever the umask: a trusted directory, then a
        // directory where the manifest goes.
        for made in [&file_dir, &manifest] {
            std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o755)
                .create(made)
                .expect("a directory");
            std::fs::set_permissions(made, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        match check_release(&release, &manifest, me(), &token()) {
            Ok(ReleaseCheck::Untrusted(why)) => {
                assert_eq!(why, format!("{}: not a regular file", manifest.display()));
            }
            other => panic!("used: {other:?}"),
        }
    }

    /// A manifest that can't be written (its directory's place taken by a
    /// file, or its own by a directory) is the write's error, naming the
    /// manifest; no temporary file is left beside it.
    #[test]
    fn a_manifest_that_cant_be_written_names_it() {
        let (dir, release, _) = release();
        std::fs::write(dir.path().join("manifests"), "not a directory").expect("file");
        let manifest = dir.path().join("manifests/python-3-12-15-x.json");
        let named = |err: &WriteError| {
            assert!(
                matches!(err, WriteError::Write { path, .. } if *path == manifest),
                "{err:?}"
            );
            assert!(
                err.to_string().starts_with(&format!(
                    "write Python release manifest {}: ",
                    manifest.display()
                )),
                "{err}"
            );
        };
        named(&write_release_manifest(&release, &manifest, me()).expect_err("not written"));
        std::fs::remove_file(dir.path().join("manifests")).expect("rm");
        std::fs::create_dir_all(manifest.join("taken")).expect("in the way");
        named(&write_release_manifest(&release, &manifest, me()).expect_err("not renamed"));
        let left: Vec<_> = std::fs::read_dir(dir.path().join("manifests"))
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(left, ["python-3-12-15-x.json"]);
    }

    /// A release that isn't sb's (here: sb's owner taken as another uid,
    /// as after a swap by `/srv/python`'s owner) gets no manifest.
    #[test]
    fn a_release_that_isnt_sbs_gets_no_manifest() {
        let (_dir, release, manifest) = release();
        let err = write_release_manifest(&release, &manifest, me() + 1).expect_err("refused");
        assert!(err.to_string().contains("owned by uid"), "{err}");
        assert!(!manifest.exists());
    }

    #[test]
    fn a_release_without_a_manifest_is_not_checked() {
        let (_dir, release, manifest) = release();
        assert_eq!(
            check_release(&release, &manifest, me(), &token()),
            Ok(ReleaseCheck::NoManifest)
        );
        std::fs::create_dir_all(manifest.parent().expect("dir")).expect("dir");
        assert_eq!(
            check_release(&release, &manifest, me(), &token()),
            Ok(ReleaseCheck::NoManifest)
        );
    }

    /// A manifest someone else could have rewritten is never used: owned
    /// by another user (here: sb's owner taken as another uid), writable
    /// by group or others, in such a directory, or a link.
    #[test]
    fn an_untrusted_manifest_is_reported_not_used() {
        let (dir, release, manifest) = release();
        write_release_manifest(&release, &manifest, me()).expect("written");
        let untrusted = |owner: u32| match check_release(&release, &manifest, owner, &token()) {
            Ok(ReleaseCheck::Untrusted(why)) => why,
            other => panic!("trusted: {other:?}"),
        };
        let other = me() + 1;
        assert!(
            untrusted(other).contains(&format!("not {other}")),
            "{}",
            untrusted(other)
        );
        let chmod = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        chmod(&manifest, 0o664);
        assert!(untrusted(me()).contains("writable by group or others (mode 664)"));
        chmod(&manifest, 0o644);
        let parent = manifest.parent().expect("dir");
        chmod(parent, 0o777);
        assert!(untrusted(me()).contains("writable by group or others (mode 777)"));
        chmod(parent, 0o755);
        let elsewhere = dir.path().join("elsewhere.json");
        std::fs::rename(&manifest, &elsewhere).expect("move");
        symlink(&elsewhere, &manifest).expect("link");
        assert!(untrusted(me()).contains(&manifest.display().to_string()));
    }

    /// A manifest of another schema is not used.
    #[test]
    fn a_manifest_of_another_schema_is_refused() {
        let (_dir, release, manifest) = release();
        write_release_manifest(&release, &manifest, me()).expect("written");
        let text = std::fs::read_to_string(&manifest).expect("manifest");
        std::fs::write(&manifest, text.replace("\"schema\":1", "\"schema\":2")).expect("schema");
        match check_release(&release, &manifest, me(), &token()) {
            Ok(ReleaseCheck::Untrusted(why)) => assert!(why.contains("schema 2, not 1"), "{why}"),
            other => panic!("used: {other:?}"),
        }
    }

    /// As root (the gate runs the tests so): a manifest owned by another
    /// user is refused for root.
    #[test]
    fn as_root_a_manifest_owned_by_nobody_is_refused() {
        if me() != 0 {
            return;
        }
        let (_dir, release, manifest) = release();
        write_release_manifest(&release, &manifest, me()).expect("written");
        // nobody's uid on Ubuntu (base-passwd).
        let nobody = Some(rustix::fs::Uid::from_raw(65534));
        rustix::fs::chown(&manifest, nobody, None).expect("chown");
        assert_eq!(
            check_release(&release, &manifest, 0, &token()),
            Ok(ReleaseCheck::Untrusted(format!(
                "{} is owned by uid 65534, not 0",
                manifest.display()
            )))
        );
    }
}
