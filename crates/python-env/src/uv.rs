//! The host's uv and uvx (python-env spec, step 2; sb-go's `python/uv.go`,
//! `ensureVersionWithProxyAndRuntime`, `binaryVersionWithRuntime` and
//! `extractUVBinary`): both probed, and when either isn't the uv this sb
//! pins, that release installed from astral-sh's GitHub release through
//! sb-release (R1-R6: the metadata, the verified download, the
//! activation), its two binaries read out of the archive with tar and
//! flate2 (decision 13), each checked with `--version` before it replaces
//! the host's.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::time::Duration;

use sb_release::{Arch, GithubRepo, Query, Stage};
use sb_term_ui::Step;
use tokio_util::sync::CancellationToken;

use crate::Env;
use crate::isolate::{self, IsolateError, Kind};

/// uv's releases (decision 4: uv stays a host CLI, from astral-sh).
pub const UV_REPO: GithubRepo = GithubRepo {
    owner: "astral-sh",
    name: "uv",
};

/// Each `--version` probe's limit (spec, difference 8).
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The archive's download cap, and each binary's in it (sb-go's 128 MiB
/// both).
const MAX_ARCHIVE: u64 = 128 << 20;
const MAX_BINARY: u64 = 128 << 20;

/// The two binaries, uv first, in the order sb-go extracts and activates
/// them.
const NAMES: [&str; 2] = ["uv", "uvx"];

/// Why uv or uvx couldn't be probed or installed, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum UvError {
    /// The probe's child couldn't be prepared.
    #[error(transparent)]
    Isolate(#[from] IsolateError),
    /// The probe couldn't run or failed (exec's text).
    #[error("{0}")]
    Run(String),
    /// Its output isn't `<name> <version> ...` (sb-go's text).
    #[error("unexpected {name} version output {output:?}")]
    Output { name: &'static str, output: String },
    /// A binary that failed its probe can't even be looked at.
    #[error("error checking installed {name}: {cause}")]
    Check {
        name: &'static str,
        cause: io::Error,
    },
    /// The release's metadata, or its asset, wasn't usable.
    #[error("error fetching uv {version} release metadata: {cause}")]
    Metadata {
        version: String,
        cause: sb_release::Error,
    },
    /// The archive's download failed or didn't verify.
    #[error("error downloading uv: {0}")]
    Download(sb_release::Error),
    /// The layout puts uv and uvx in different directories.
    #[error("uv and uvx destinations must share a directory")]
    Destinations,
    /// A file or directory of the install couldn't be made.
    #[error("{what}: {cause}")]
    Io { what: String, cause: io::Error },
    /// A binary couldn't be read out of the archive.
    #[error("error extracting {name} binary: {cause}")]
    Extract {
        name: &'static str,
        cause: ExtractError,
    },
    /// A staged binary's own probe failed.
    #[error("error verifying staged {name}: {cause}")]
    Verify {
        name: &'static str,
        cause: Box<UvError>,
    },
    /// A staged binary is another release than the one asked for.
    #[error("downloaded {name} reports version {found}, expected {expected}")]
    Staged {
        name: &'static str,
        found: String,
        expected: String,
    },
    /// Ctrl-C during the probes: nothing was asked of GitHub.
    #[error("interrupted")]
    Interrupted,
    /// A verified binary couldn't replace the host's.
    #[error("error activating {name} {version}: {cause}")]
    Activate {
        name: &'static str,
        version: String,
        cause: sb_release::Error,
    },
}

/// Why one binary couldn't be read out of the archive (sb-go's
/// `extractUVBinary` texts).
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("error opening tarball: {0}")]
    Open(io::Error),
    #[error("error creating gzip reader: {0}")]
    Gzip(io::Error),
    #[error("error reading tar: {0}")]
    Read(io::Error),
    /// The first entry of that name is a link, a directory or another
    /// kind.
    #[error("{name} archive entry is not a regular file")]
    NotRegular { name: &'static str },
    #[error("{name} archive entry has invalid size {size}")]
    Size { name: &'static str, size: u64 },
    #[error("error opening staged {name} binary: {cause}")]
    Create {
        name: &'static str,
        cause: io::Error,
    },
    // sb-go says "uv binary" for uvx too.
    #[error("error writing uv binary: {0}")]
    Write(io::Error),
    #[error("error syncing uv binary: {0}")]
    Sync(io::Error),
    #[error("{name} archive entry size mismatch: expected {expected} bytes, got {written}")]
    Mismatch {
        name: &'static str,
        expected: u64,
        written: u64,
    },
    #[error("{name} binary not found in tarball")]
    NotFound { name: &'static str },
}

/// The version in `output`, which must start with `name` (sb-go splits
/// the trimmed output at white space: `uv 0.13.4 (abc 2026-01-01)`).
fn parse_version(name: &'static str, output: &str) -> Result<String, UvError> {
    let output = output.trim();
    let mut fields = output.split_whitespace();
    match (fields.next(), fields.next()) {
        (Some(first), Some(version)) if first == name => Ok(version.to_string()),
        _ => Err(UvError::Output {
            name,
            output: output.to_string(),
        }),
    }
}

/// The version `program` reports as `name`: `<program> --no-config
/// --version`, isolated as every uv child, captured within 10 s.
///
/// # Errors
///
/// [`UvError`]: the child can't be prepared or run, fails, or prints
/// something else.
pub async fn version_of(
    env: &Env<'_>,
    program: &Path,
    name: &'static str,
    token: &CancellationToken,
) -> Result<String, UvError> {
    let probe = env
        .isolated(Kind::Uv { cache: false }, program)?
        .map(|cmd| cmd.arg("--version").timeout(PROBE_TIMEOUT));
    let output = crate::captured(&probe.cmd, token)
        .await
        .map_err(UvError::Run)?;
    parse_version(name, &output)
}

/// Whether the layout's uv and uvx both report `env.uv_version` (spec,
/// step 2, its first bullet). A binary that fails its probe is replaced,
/// unless it can't even be looked at (sb-go stops there).
///
/// # Errors
///
/// [`UvError::Check`] when a binary that failed its probe can't be
/// `stat`ed for a reason other than its absence.
pub async fn is_installed(env: &Env<'_>, token: &CancellationToken) -> Result<bool, UvError> {
    let mut installed = true;
    for (path, name) in [(&env.layout.uv, NAMES[0]), (&env.layout.uvx, NAMES[1])] {
        match version_of(env, path, name, token).await {
            Ok(found) if found == env.uv_version => {}
            Ok(found) => {
                tracing::info!(name, %found, "replacing another release");
                installed = false;
            }
            Err(err) => {
                if let Err(cause) = std::fs::metadata(path)
                    && cause.kind() != io::ErrorKind::NotFound
                {
                    return Err(UvError::Check { name, cause });
                }
                tracing::info!(name, cause = %err, "replacing an unusable or missing binary");
                installed = false;
            }
        }
    }
    Ok(installed)
}

/// "Ensuring uv <v> is installed" (spec, step 2): nothing when both
/// binaries are the pinned release; otherwise that release from
/// astral-sh, installed over them.
///
/// # Errors
///
/// [`UvError`]: the first part that fails, with sb-go's text (a Ctrl-C
/// shows in the metadata's or download's); the host's binaries are as
/// they were unless an activation itself failed.
pub async fn ensure_installed(step: &mut Step, env: &Env<'_>) -> Result<(), UvError> {
    let token = step.token();
    if is_installed(env, &token).await? {
        return Ok(());
    }
    // A Ctrl-C during the probes fails them; it isn't a reason to install.
    if token.is_cancelled() {
        return Err(UvError::Interrupted);
    }
    install(step, env, &token).await
}

/// uv's archive for `arch` (decision 8; sb-go hard-codes `x86_64`).
#[must_use]
pub fn asset_name(arch: Arch) -> String {
    format!("uv-{}-unknown-linux-gnu.tar.gz", arch.rust_name())
}

/// The pinned release installed (sb-go's `ensureVersionWithProxyAndRuntime`
/// after its probes): its metadata by tag, the archive downloaded and
/// verified into a scratch directory, uv and uvx extracted to fixed names
/// in a `.uv-install-*` directory beside the targets, each staged binary's
/// `--version` checked, then each moved beside its target and activated,
/// uv first.
async fn install(step: &mut Step, env: &Env<'_>, token: &CancellationToken) -> Result<(), UvError> {
    let version = env.uv_version;
    let targets = [&env.layout.uv, &env.layout.uvx];
    let dir = match targets.map(|target| target.parent()) {
        [Some(uv), Some(uvx)] if uv == uvx => uv,
        _ => return Err(UvError::Destinations),
    };
    let asset = asset_name(env.arch);
    let release = env
        .release
        .release(UV_REPO, Query::Tag(version.to_owned()), &asset, step)
        .await
        .map_err(|cause| UvError::Metadata {
            version: version.to_owned(),
            cause,
        })?;
    tracing::info!(url = %release.asset.url(), "downloading uv");
    let scratch = isolate::scratch(env.layout)?;
    let stage = Stage {
        dir: scratch.path(),
        prefix: "uv-",
        max_size: MAX_ARCHIVE,
    };
    let archive = env
        .release
        .download(&release.asset, stage, step)
        .await
        .map_err(UvError::Download)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
        .map_err(io_failed("error creating uv destination directory"))?;
    // tempfile's directory, 0700, removed with everything left in it
    // (<https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.tempdir_in>).
    let staging = tempfile::Builder::new()
        .prefix(".uv-install-")
        .tempdir_in(dir)
        .map_err(io_failed("error creating uv staging directory"))?;
    // Decompressing up to twice 128 MiB is blocking work; sb's runtime has
    // one thread, which also draws the steps
    // (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>).
    let (from, into) = (archive.path().to_path_buf(), staging.path().to_path_buf());
    let staged = tokio::task::spawn_blocking(move || {
        let mut staged = Vec::with_capacity(NAMES.len());
        for name in NAMES {
            let path = into.join(name);
            extract(&from, name, &path, MAX_BINARY)
                .map_err(|cause| UvError::Extract { name, cause })?;
            staged.push(path);
        }
        Ok::<_, UvError>(staged)
    })
    .await
    // A panic in the extraction is resumed here, as `JoinError::into_panic`'s
    // example does (sb-host's `unpanicked`); the other `JoinError`, a
    // cancelled task, needs a runtime shut down under a polled future, which
    // sb's `block_on` never does
    // (<https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinError.html#method.into_panic>).
    .unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()))?;
    drop(archive);
    for (staged, name) in staged.iter().zip(NAMES) {
        let found = version_of(env, staged, name, token)
            .await
            .map_err(|cause| UvError::Verify {
                name,
                cause: Box::new(cause),
            })?;
        if found != version {
            return Err(UvError::Staged {
                name,
                found,
                expected: version.to_owned(),
            });
        }
    }
    // Moved beside the targets first: release's `activate` renames
    // within one directory. Each is removed when dropped unless
    // activated.
    let mut ready = Vec::with_capacity(NAMES.len());
    for (staged, name) in staged.iter().zip(NAMES) {
        let file = tempfile::Builder::new()
            .prefix(&format!(".{name}-"))
            .tempfile_in(dir)
            .map_err(io_failed(format!(
                "error creating staged {name} activation file"
            )))?
            .into_temp_path();
        std::fs::rename(staged, &file).map_err(io_failed(format!(
            "error preparing staged {name} for activation"
        )))?;
        ready.push(file);
    }
    // Nothing awaits from the last probe on: a Ctrl-C now is seen by the
    // next step, with the verified binaries in place.
    for ((file, target), name) in ready.into_iter().zip(targets).zip(NAMES) {
        sb_release::activate(file, target, 0o755).map_err(|cause| UvError::Activate {
            name,
            version: version.to_owned(),
            cause,
        })?;
    }
    tracing::info!(version, "installed uv and uvx");
    Ok(())
}

/// An I/O failure of the install, after `what`.
fn io_failed(what: impl Into<String>) -> impl FnOnce(io::Error) -> UvError {
    let what = what.into();
    move |cause| UvError::Io { what, cause }
}

/// The first entry of the gzip'd tar at `archive` named `name` or ending
/// in `/<name>`, written to `staged` (created here, 0755), never to the
/// entry's own path: a regular file of 1 to `max` bytes, its written size
/// its entry's (sb-go's `extractUVBinary`). Read with flate2's
/// `MultiGzDecoder` (Go's `gzip.Reader` reads concatenated members too,
/// <https://docs.rs/flate2/1.1.10/flate2/read/struct.MultiGzDecoder.html>)
/// and tar's `Archive::entries`
/// (<https://docs.rs/tar/0.4.46/tar/struct.Archive.html#method.entries>),
/// the name from `path_bytes`, which holds GNU and PAX long names
/// (<https://docs.rs/tar/0.4.46/tar/struct.Entry.html#method.path_bytes>).
fn extract(
    archive: &Path,
    name: &'static str,
    staged: &Path,
    max: u64,
) -> Result<(), ExtractError> {
    let file = File::open(archive).map_err(ExtractError::Open)?;
    let mut gzip = flate2::read::MultiGzDecoder::new(file);
    // The header is parsed when the decoder is made; a bad one shows as
    // no header, its error on the first read
    // (<https://docs.rs/flate2/1.1.10/flate2/read/struct.MultiGzDecoder.html#method.header>).
    if gzip.header().is_none() {
        let cause = gzip
            .read(&mut [0])
            .err()
            .unwrap_or_else(|| io::Error::other("invalid gzip header"));
        return Err(ExtractError::Gzip(cause));
    }
    let suffix = format!("/{name}");
    let mut tar = tar::Archive::new(gzip);
    for entry in tar.entries().map_err(ExtractError::Read)? {
        let mut entry = entry.map_err(ExtractError::Read)?;
        let path = entry.path_bytes();
        if *path != *name.as_bytes() && !path.ends_with(suffix.as_bytes()) {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(ExtractError::NotRegular { name });
        }
        let size = entry.size();
        if size == 0 || size > max {
            return Err(ExtractError::Size { name, size });
        }
        // Why not tar's `Entry::unpack`
        // (<https://docs.rs/tar/0.4.46/tar/struct.Entry.html#method.unpack>):
        // it takes the archive's mode, replaces an existing file, sets the
        // mtime and has no text with the two sizes sb-go's mismatch names.
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(staged)
            .map_err(|cause| ExtractError::Create { name, cause })?;
        // tar's entry reader stops at its size, so only a short read can
        // differ (sb-go reads `size + 1` to catch a longer one).
        let written = io::copy(&mut entry, &mut out).map_err(ExtractError::Write)?;
        out.sync_all().map_err(ExtractError::Sync)?;
        if written != size {
            return Err(ExtractError::Mismatch {
                name,
                expected: size,
                written,
            });
        }
        return Ok(());
    }
    Err(ExtractError::NotFound { name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_the_second_field() {
        let got = parse_version("uv", "uv 0.13.4 (0cf2d7a 2026-11-01)\n").expect("parsed");
        assert_eq!(got, "0.13.4");
        let got = parse_version("uvx", "  uvx 0.13.4\n").expect("parsed");
        assert_eq!(got, "0.13.4");
    }

    /// sb-go's text, the output trimmed and quoted.
    #[test]
    fn other_output_is_refused() {
        for bad in ["", "uv", "uvx 0.13.4", "error: no\n"] {
            let err = parse_version("uv", bad).expect_err(bad);
            assert_eq!(
                err.to_string(),
                format!("unexpected uv version output {:?}", bad.trim())
            );
        }
    }

    #[test]
    fn the_asset_is_the_builds_architecture() {
        assert_eq!(
            asset_name(Arch::Amd64),
            "uv-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            asset_name(Arch::Arm64),
            "uv-aarch64-unknown-linux-gnu.tar.gz"
        );
    }

    /// One entry of a test archive.
    enum Entry<'a> {
        File(&'a str, &'a [u8]),
        Symlink(&'a str),
        Dir(&'a str),
    }

    /// A tar.gz of `entries`, built with tar's `Builder`
    /// (<https://docs.rs/tar/0.4.46/tar/struct.Builder.html>).
    fn archive(entries: &[Entry<'_>]) -> Vec<u8> {
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gzip);
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            match entry {
                Entry::File(path, body) => {
                    header.set_size(body.len() as u64);
                    tar.append_data(&mut header, path, *body).expect("file");
                }
                Entry::Symlink(path) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    tar.append_link(&mut header, path, "/bin/sh").expect("link");
                }
                Entry::Dir(path) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    tar.append_data(&mut header, path, io::empty())
                        .expect("dir");
                }
            }
        }
        tar.into_inner().expect("tar").finish().expect("gzip")
    }

    /// `bytes` as an archive in a temporary directory, and `name`
    /// extracted from it to `staged` there (cap 16 bytes): the result and
    /// what was written.
    fn extracted(bytes: &[u8], name: &'static str) -> (Result<(), ExtractError>, Option<Vec<u8>>) {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("uv.tar.gz");
        std::fs::write(&path, bytes).expect("archive");
        let staged = dir.path().join("staging").join(name);
        std::fs::create_dir(staged.parent().expect("parent")).expect("staging");
        let result = extract(&path, name, &staged, 16);
        let written = std::fs::read(&staged).ok();
        (result, written)
    }

    #[test]
    fn nested_and_top_level_names_are_extracted() {
        let bytes = archive(&[
            Entry::Dir("uv-x86_64-unknown-linux-gnu/"),
            Entry::File("uv-x86_64-unknown-linux-gnu/uvx", b"the uvx"),
            Entry::File("uv-x86_64-unknown-linux-gnu/uv", b"the uv"),
        ]);
        let (result, written) = extracted(&bytes, "uv");
        result.expect("uv");
        assert_eq!(written.as_deref(), Some(b"the uv".as_slice()));
        let (result, written) = extracted(&bytes, "uvx");
        result.expect("uvx");
        assert_eq!(written.as_deref(), Some(b"the uvx".as_slice()));
        let top = archive(&[Entry::File("uv", b"top uv")]);
        let (result, written) = extracted(&top, "uv");
        result.expect("top-level uv");
        assert_eq!(written.as_deref(), Some(b"top uv".as_slice()));
    }

    /// Only the name or a `/<name>` suffix counts: `uvx` isn't `uv`, nor
    /// is `not-uv`; the first match is the one taken.
    #[test]
    fn only_the_exact_name_matches_and_the_first_wins() {
        let bytes = archive(&[
            Entry::File("dir/uvx", b"uvx"),
            Entry::File("dir/not-uv", b"not"),
            Entry::File("a/uv", b"first"),
            Entry::File("b/uv", b"second"),
        ]);
        let (result, written) = extracted(&bytes, "uv");
        result.expect("uv");
        assert_eq!(written.as_deref(), Some(b"first".as_slice()));
    }

    /// A `../uv` entry is written to the staged name, nowhere else.
    #[test]
    fn an_entry_outside_the_archive_is_written_to_the_staged_name() {
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(gzip);
        // tar's `set_path` refuses `..`; the name field is written raw, as
        // a hostile archive would hold it.
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..5].copy_from_slice(b"../uv");
        header.set_size(4);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append(&header, b"evil".as_slice()).expect("entry");
        let bytes = tar.into_inner().expect("tar").finish().expect("gzip");
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("uv.tar.gz");
        std::fs::write(&path, bytes).expect("archive");
        let staging = dir.path().join("staging");
        std::fs::create_dir(&staging).expect("staging");
        extract(&path, "uv", &staging.join("uv"), 16).expect("extracted");
        assert_eq!(std::fs::read(staging.join("uv")).expect("staged"), b"evil");
        assert!(!dir.path().join("uv").exists());
    }

    #[test]
    fn a_link_or_directory_named_uv_is_refused() {
        for entries in [
            [Entry::Symlink("dir/uv")],
            [Entry::Dir("dir/uv")],
            [Entry::Symlink("uv")],
        ] {
            let (result, written) = extracted(&archive(&entries), "uv");
            assert_eq!(
                result.expect_err("not regular").to_string(),
                "uv archive entry is not a regular file"
            );
            assert_eq!(written, None);
        }
        // A directory spelt `uv/` is not `uv`: the binary is missing.
        let (result, _) = extracted(&archive(&[Entry::Dir("uv/")]), "uv");
        assert_eq!(
            result.expect_err("missing").to_string(),
            "uv binary not found in tarball"
        );
    }

    #[test]
    fn an_empty_or_oversized_entry_is_refused() {
        let (result, written) = extracted(&archive(&[Entry::File("d/uv", b"")]), "uv");
        assert_eq!(
            result.expect_err("empty").to_string(),
            "uv archive entry has invalid size 0"
        );
        assert_eq!(written, None);
        let at_cap = [7u8; 16];
        let (result, _) = extracted(&archive(&[Entry::File("d/uv", &at_cap)]), "uv");
        result.expect("at the cap");
        let over = [7u8; 17];
        let (result, written) = extracted(&archive(&[Entry::File("d/uv", &over)]), "uv");
        assert_eq!(
            result.expect_err("oversized").to_string(),
            "uv archive entry has invalid size 17"
        );
        assert_eq!(written, None);
    }

    #[test]
    fn a_missing_uvx_is_named() {
        let (result, _) = extracted(&archive(&[Entry::File("d/uv", b"uv")]), "uvx");
        assert_eq!(
            result.expect_err("missing").to_string(),
            "uvx binary not found in tarball"
        );
    }

    /// A tar cut inside the binary's data, in a whole gzip stream: the
    /// entry reads short, and sb-go's text names both sizes.
    #[test]
    fn a_short_entry_is_a_size_mismatch() {
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(16);
        header.set_mode(0o755);
        tar.append_data(&mut header, "d/uv", [b'x'; 16].as_slice())
            .expect("entry");
        let whole = tar.into_inner().expect("tar");
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        io::Write::write_all(&mut gzip, &whole[..512 + 8]).expect("gzip");
        let (result, _) = extracted(&gzip.finish().expect("gzip"), "uv");
        assert_eq!(
            result.expect_err("short").to_string(),
            "uv archive entry size mismatch: expected 16 bytes, got 8"
        );
    }

    /// A gzip stream cut inside the binary's data fails as the copy's
    /// read error: deflate's stored blocks keep the tar's bytes in place,
    /// so the cut lands 8 bytes into the entry's 16 (gzip's 10-byte header,
    /// the block's 5, tar's 512).
    #[test]
    fn a_truncated_gzip_stream_fails() {
        let gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
        let mut tar = tar::Builder::new(gzip);
        let mut header = tar::Header::new_gnu();
        header.set_size(16);
        header.set_mode(0o755);
        tar.append_data(&mut header, "d/uv", [b'x'; 16].as_slice())
            .expect("entry");
        let whole = tar.into_inner().expect("tar").finish().expect("gzip");
        let (result, _) = extracted(&whole[..10 + 5 + 512 + 8], "uv");
        let err = result.expect_err("truncated").to_string();
        assert!(err.starts_with("error writing uv binary: "), "{err}");
    }

    #[test]
    fn a_file_that_isnt_gzip_or_is_missing_is_named() {
        let (result, _) = extracted(b"not a gzip archive", "uv");
        let err = result.expect_err("not gzip").to_string();
        assert!(err.starts_with("error creating gzip reader: "), "{err}");
        let dir = tempfile::tempdir().expect("dir");
        let err = extract(&dir.path().join("gone"), "uv", &dir.path().join("uv"), 16)
            .expect_err("missing");
        assert!(
            err.to_string().starts_with("error opening tarball: "),
            "{err}"
        );
    }

    /// The staged name is made new: an existing file there is an error,
    /// never written through.
    #[test]
    fn the_staged_file_is_made_new() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("uv.tar.gz");
        std::fs::write(&path, archive(&[Entry::File("d/uv", b"uv")])).expect("archive");
        let staged = dir.path().join("uv");
        std::fs::write(&staged, "already").expect("existing");
        let err = extract(&path, "uv", &staged, 16).expect_err("exists");
        assert!(
            err.to_string()
                .starts_with("error opening staged uv binary: "),
            "{err}"
        );
        assert_eq!(std::fs::read(&staged).expect("kept"), b"already");
    }
}
