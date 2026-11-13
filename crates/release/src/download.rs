//! Verified downloads (release spec, "Download"): the asset streamed into
//! a staged file next to where it will go, its size and SHA-256 checked as
//! it arrives, then synced and closed, so nothing unverified is ever left
//! for a caller to run. sb-go's `downloadVerified` (release/asset.go) is
//! the reference for the steps and the texts.

use std::path::Path;

use reqwest::StatusCode;
use sb_term_ui::Step;
use sha2::{Digest, Sha256};
use tempfile::TempPath;
use tokio::io::AsyncWriteExt;

use crate::lookup::causes;
use crate::{Asset, Error, Releases};

/// Where and how a download is staged: in `dir` (the target's directory,
/// so the final rename stays on one filesystem), the file name starting
/// with `prefix` (a constant without `/`), refused when the asset is
/// larger than `max_size`. `dir` must be writable by root only: the staged
/// file is checked as it is written, and activated by its path.
#[derive(Clone, Copy, Debug)]
pub struct Stage<'a> {
    pub dir: &'a Path,
    pub prefix: &'a str,
    pub max_size: u64,
}

/// A verified download: its size and digest matched, synced and closed (an
/// open write handle would make running it fail with ETXTBSY). Removed
/// when dropped, unless activated.
#[derive(Debug)]
pub struct Staged(TempPath);

impl Staged {
    /// The staged file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl From<Staged> for TempPath {
    fn from(staged: Staged) -> Self {
        staged.0
    }
}

impl Releases {
    /// Download `asset` into a file staged as `stage` says, checking its
    /// size and SHA-256 as it streams: past the asset's size it stops
    /// (`larger than expected`); short at the end, or with another digest,
    /// it fails. Whatever fails leaves nothing staged. The step's token
    /// stops it. No total timeout: each read has the client's own.
    ///
    /// # Errors
    ///
    /// [`Error::Download`] with sb-go's texts, except where the spec words
    /// them ("larger than expected", "digest mismatch"),
    /// [`Error::Interrupted`] when
    /// the step's token is cancelled.
    pub async fn download(
        &self,
        asset: &Asset,
        stage: Stage<'_>,
        step: &mut Step,
    ) -> Result<Staged, Error> {
        let name = asset.name();
        if asset.size() > stage.max_size {
            return Err(Error::Download(format!(
                "release asset {name:?} exceeds the {} byte limit",
                stage.max_size
            )));
        }
        let token = step.token();
        token
            .run_until_cancelled(self.stage(asset, stage))
            .await
            .unwrap_or(Err(Error::Interrupted))
    }

    async fn stage(&self, asset: &Asset, stage: Stage<'_>) -> Result<Staged, Error> {
        let name = asset.name();
        let failed = Error::Download;
        let mut response = self
            .client
            .get(asset.url().clone())
            .send()
            .await
            .map_err(|err| failed(format!("download release asset {name:?}: {}", causes(err))))?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(failed(format!(
                "download release asset {name:?}: unexpected HTTP status {status}"
            )));
        }
        // tempfile's builder in the caller's directory
        // (<https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.tempfile_in>);
        // the path half removes the file when dropped, so every early
        // return below leaves nothing staged
        // (<https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.into_parts>).
        let (file, path) = tempfile::Builder::new()
            .prefix(stage.prefix)
            .tempfile_in(stage.dir)
            .map_err(io_failed("create staged release asset".to_owned()))?
            .into_parts();
        // Written through tokio's file, on its blocking pool, so the
        // runner keeps drawing
        // (<https://docs.rs/tokio/1.54.1/tokio/fs/struct.File.html#method.from_std>).
        let mut file = tokio::fs::File::from_std(file);
        // Each chunk counted and hashed before it is written. Why not
        // `tokio::io::copy` from a stream reader with tokio-util's
        // `InspectReader` and `take`: it needs reqwest's `stream` feature
        // and tokio-util's `io`, and loses sb-go's two size texts;
        // `Response::chunk` is the spec's API.
        let mut digest = Sha256::new();
        let mut written: u64 = 0;
        loop {
            let chunk = response.chunk().await.map_err(|err| {
                failed(format!(
                    "copy release asset {name:?} into staging: read response body: {}",
                    causes(err)
                ))
            })?;
            let Some(chunk) = chunk else { break };
            written = written.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if written > asset.size() {
                return Err(failed(format!(
                    "release asset {name:?} is larger than expected: more than {} bytes",
                    asset.size()
                )));
            }
            digest.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(io_failed(write(name)))?;
        }
        if written < asset.size() {
            return Err(failed(format!(
                "release asset {name:?} size mismatch: expected {} bytes, got {written}",
                asset.size()
            )));
        }
        if digest.finalize().as_slice() != asset.sha256() {
            return Err(failed(format!("release asset {name:?} digest mismatch")));
        }
        // tokio's file hands each write to its blocking pool, and an error
        // in the last one only comes back from `flush`: `sync_all` waits
        // for it but drops its error (tokio 1.54.1, `File::sync_all` and
        // `complete_inflight`), which would leave a file short on disk
        // passed as verified. Hence `flush` first, as tokio's `File` docs
        // ask before a drop
        // (<https://docs.rs/tokio/1.54.1/tokio/fs/struct.File.html>).
        file.flush().await.map_err(io_failed(write(name)))?;
        file.sync_all()
            .await
            .map_err(io_failed(format!("sync staged release asset {name:?}")))?;
        // Closed here, so the file can be run (an open write handle gives
        // ETXTBSY): `into_std` waits for every operation in flight
        // (<https://docs.rs/tokio/1.54.1/tokio/fs/struct.File.html#method.into_std>).
        drop(file.into_std().await);
        Ok(Staged(path))
    }
}

/// The text of a failed write to the staged file.
fn write(name: &str) -> String {
    format!("copy release asset {name:?} into staging: write staged file")
}

/// An I/O failure while staging, after `what`.
fn io_failed(what: String) -> impl FnOnce(std::io::Error) -> Error {
    move |err| Error::Download(format!("{what}: {err}"))
}
