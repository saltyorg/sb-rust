//! bench's disk write test (ops spec, "`sb bench`", 2.; decision 12), in
//! process: blocks of zeros written to a temporary file in the working
//! directory, then `File::sync_data` (fdatasync, as bench.sh's `dd
//! conv=fdatasync`), timed from the first write to the end of the sync.

use std::io::{self, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use sb_cli_core::cancel::Shutdown;
use sb_term_ui::{Cell, Table};

/// The least free space the test runs with (host's disk-space floor, ops
/// decision 12).
pub const MIN_FREE: u64 = 2 * 1024 * 1024 * 1024;

/// How many runs the test makes.
pub const RUNS: usize = 3;

/// The row when the working directory's filesystem has under 2 GiB free.
pub const NO_SPACE: &str = "Not enough space for the disk test";

/// The row when the test file can't be created there.
pub const NO_WRITE: &str = "No write permission in the working directory";

/// The row when the free space can't be read (bench.sh's "Unable to
/// determine available space", in sb's words).
pub const NO_FREE_SPACE: &str = "Unable to determine available space for the disk test";

/// A run's row when it failed (as a failed Speedtest node's).
pub const FAILED: &str = "Test failed";

/// One run's size: `blocks` writes of `block` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub blocks: usize,
    pub block: usize,
}

impl Size {
    /// The bytes one run writes.
    #[must_use]
    pub fn bytes(self) -> usize {
        self.blocks.saturating_mul(self.block)
    }
}

impl Default for Size {
    /// bench.sh's: 2048 blocks of 512 KiB, 1 GiB a run.
    fn default() -> Self {
        Self {
            blocks: 2048,
            block: 512 * 1024,
        }
    }
}

/// Why a run didn't measure.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The test file couldn't be created in the directory.
    #[error("creating the test file: {0}")]
    Create(io::Error),
    /// A write or the sync failed.
    #[error("writing the test file: {0}")]
    Write(io::Error),
    /// Stopped between blocks.
    #[error("interrupted")]
    Interrupted,
}

/// One run in `dir`: `size`'s blocks of zeros written to a new temporary
/// file, then synced, and the file removed. `keep_going` is asked before
/// each block; when it says no the run stops there. The file is removed
/// on every path: tempfile's `NamedTempFile` deletes it when dropped, and
/// while the run lasts a hook removes it before a second Ctrl-C's exit.
/// The file is created `0600` and exclusively (`O_EXCL`), tempfile's
/// defaults
/// (<https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.tempfile_in>,
/// <https://docs.rs/tempfile/3.27.0/tempfile/struct.Builder.html#method.permissions>).
/// Blocking: run it on the blocking pool (cli-core's `spawn::blocking`).
///
/// # Errors
///
/// [`RunError`]: the file can't be created, a write or the sync fails,
/// or `keep_going` stopped it.
pub fn run(
    dir: &Path,
    size: Size,
    shutdown: &Shutdown,
    keep_going: impl Fn() -> bool,
) -> Result<Duration, RunError> {
    let file = tempfile::Builder::new()
        .prefix(".sb-bench-")
        .tempfile_in(dir)
        .map_err(RunError::Create)?;
    let path = file.path().to_path_buf();
    // Tiny, as cli-core asks of a forced-exit hook: one unlink.
    let _remove = shutdown.on_forced_exit(move || {
        let _ = std::fs::remove_file(&path);
    });
    let zeros = vec![0u8; size.block];
    let started = Instant::now();
    for _ in 0..size.blocks {
        if !keep_going() {
            return Err(RunError::Interrupted);
        }
        file.as_file().write_all(&zeros).map_err(RunError::Write)?;
    }
    file.as_file().sync_data().map_err(RunError::Write)?;
    let elapsed = started.elapsed();
    // The run measured; a failed unlink is logged, not a failed run.
    if let Err(err) = file.close() {
        tracing::warn!(error = %err, "disk test file not removed");
    }
    Ok(elapsed)
}

/// MB/s (10^6 bytes, as dd reports) for `bytes` written in `elapsed`.
#[must_use]
pub fn rate(bytes: usize, elapsed: Duration) -> f64 {
    // `u32` converts to `f64` exactly; a run is far under 4 GiB.
    let bytes = f64::from(u32::try_from(bytes).unwrap_or(u32::MAX));
    bytes / elapsed.as_secs_f64().max(f64::MIN_POSITIVE) / 1e6
}

/// How the test went.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// It didn't run: the row's text.
    Skipped(&'static str),
    /// Each run's MB/s, none for a failed run.
    Runs(Vec<Option<f64>>),
}

/// The Disk write table: `1st run` to `3rd run` and `Average` (of the runs
/// that measured), one decimal, right-aligned; or the one row saying why
/// it didn't run.
#[must_use]
pub fn table(outcome: &Outcome) -> Table {
    let mut table = Table::spanning("Disk write").right_align(1);
    match outcome {
        Outcome::Skipped(why) => table.row([Cell::new("Skipped"), Cell::new(*why)]),
        Outcome::Runs(runs) => {
            let shown =
                |rate: Option<f64>| rate.map_or(FAILED.to_owned(), |r| format!("{r:.1} MB/s"));
            for (label, rate) in ["1st run", "2nd run", "3rd run"].iter().zip(runs) {
                table.row([Cell::new(*label), Cell::new(shown(*rate))]);
            }
            let measured: Vec<f64> = runs.iter().flatten().copied().collect();
            let average = (!measured.is_empty()).then(|| {
                measured.iter().sum::<f64>()
                    / f64::from(u32::try_from(measured.len()).unwrap_or(u32::MAX))
            });
            table.row([Cell::new("Average"), Cell::new(shown(average))]);
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use std::cell::Cell as Counter;
    use std::os::unix::fs::PermissionsExt as _;

    use sb_cli_core::theme::Theme;

    use super::*;

    const SMALL: Size = Size {
        blocks: 4,
        block: 4096,
    };

    fn entries(dir: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect()
    }

    #[test]
    fn the_default_run_is_one_gib_in_512_kib_blocks() {
        assert_eq!(
            Size::default(),
            Size {
                blocks: 2048,
                block: 524_288,
            }
        );
        assert_eq!(Size::default().bytes(), 1 << 30);
        assert_eq!(MIN_FREE, 2 << 30);
        assert_eq!(RUNS, 3);
    }

    #[test]
    fn a_run_writes_every_block_into_a_private_file_and_removes_it() {
        let dir = tempfile::tempdir().expect("dir");
        let asked = Counter::new(0);
        let elapsed = run(dir.path(), SMALL, &Shutdown::new(), || {
            // During the run: one file, 0600, holding the blocks so far.
            let files = entries(dir.path());
            assert_eq!(files.len(), 1, "{files:?}");
            let meta = std::fs::metadata(&files[0]).expect("meta");
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
            assert_eq!(meta.len(), asked.get() * 4096);
            assert!(
                files[0]
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".sb-bench-"))
            );
            asked.set(asked.get() + 1);
            true
        })
        .expect("run");
        assert_eq!(asked.get(), 4);
        assert!(elapsed > Duration::ZERO);
        assert_eq!(entries(dir.path()), Vec::<std::path::PathBuf>::new());
    }

    #[test]
    fn stopping_between_blocks_removes_the_file() {
        let dir = tempfile::tempdir().expect("dir");
        let asked = Counter::new(0);
        let err = run(dir.path(), SMALL, &Shutdown::new(), || {
            asked.set(asked.get() + 1);
            asked.get() < 3
        })
        .expect_err("stopped");
        assert!(matches!(err, RunError::Interrupted), "{err}");
        assert_eq!(asked.get(), 3);
        assert_eq!(entries(dir.path()), Vec::<std::path::PathBuf>::new());
    }

    #[test]
    fn a_directory_it_cannot_create_in_is_a_create_error() {
        let dir = tempfile::tempdir().expect("dir");
        // A regular file, not a directory: creation fails even for root.
        let file = dir.path().join("file");
        std::fs::write(&file, "").expect("file");
        let err = run(&file, SMALL, &Shutdown::new(), || true).expect_err("refused");
        assert!(matches!(err, RunError::Create(_)), "{err}");
        assert!(err.to_string().starts_with("creating the test file: "));
    }

    #[test]
    fn the_rate_is_in_megabytes_per_second() {
        assert!((rate(1 << 30, Duration::from_secs(2)) - 536.870_912).abs() < 1e-6);
        assert!((rate(500_000, Duration::from_millis(500)) - 1.0).abs() < 1e-9);
        // No division by zero.
        assert!(rate(1, Duration::ZERO).is_finite());
    }

    fn rendered(outcome: &Outcome) -> String {
        table(outcome).render(&Theme::plain(), false, None)
    }

    #[test]
    fn the_table_shows_each_run_and_their_average() {
        let text = rendered(&Outcome::Runs(vec![Some(100.0), Some(200.04), Some(300.0)]));
        for line in [
            "│ 1st run │ 100.0 MB/s │",
            "│ 2nd run │ 200.0 MB/s │",
            "│ 3rd run │ 300.0 MB/s │",
            "│ Average │ 200.0 MB/s │",
        ] {
            assert!(text.contains(line), "{line}\n{text}");
        }
        assert!(text.contains("Disk write"), "{text}");
    }

    #[test]
    fn a_failed_run_is_left_out_of_the_average() {
        let text = rendered(&Outcome::Runs(vec![Some(100.0), None, Some(301.0)]));
        assert!(text.contains("│ 2nd run │ Test failed │"), "{text}");
        assert!(text.contains("│ Average │  200.5 MB/s │"), "{text}");
        let text = rendered(&Outcome::Runs(vec![None, None, None]));
        assert!(text.contains("│ Average │ Test failed │"), "{text}");
    }

    #[test]
    fn a_skipped_test_has_one_row() {
        let text = rendered(&Outcome::Skipped(NO_SPACE));
        assert!(
            text.contains(&format!("│ Skipped │ {NO_SPACE} │")),
            "{text}"
        );
        assert!(!text.contains("run"), "{text}");
    }
}
