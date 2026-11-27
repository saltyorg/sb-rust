//! Disk space (host spec, "Disk space"; sb-go's `CheckDiskSpace` in
//! host/system.go): refuse to go on with under 2 GiB free on a filesystem
//! a command writes to, in sb-go's words.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

/// The least free space a checked filesystem needs: sb-go's 2 GiB.
const MIN_FREE: u64 = 2 * 1024 * 1024 * 1024;

/// What `statvfs` says about a filesystem, the fields sb reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// `f_blocks`: its size in `f_frsize` units.
    pub blocks: u64,
    /// `f_bavail`: the blocks an unprivileged user may still use.
    pub bavail: u64,
    /// `f_frsize`: the fragment size, POSIX's unit for the counts.
    pub frsize: u64,
    /// `f_fsid`: which filesystem it is.
    pub fsid: u64,
}

/// Why a disk check fails, in sb-go's words.
#[derive(Debug, thiserror::Error)]
pub enum DiskError {
    /// Under 2 GiB free.
    #[error(
        "INSUFFICIENT DISK SPACE - Install cancelled: {path} is {used:.1}% full ({free} free). \
         Free up space on {path} before continuing",
        path = path.display(),
        free = format_bytes(*free)
    )]
    Insufficient {
        /// The checked path, as resolved.
        path: PathBuf,
        /// sb-go's used percentage.
        used: f64,
        /// The free bytes.
        free: u64,
    },
    /// The filesystem couldn't be read, or reported zero blocks.
    #[error("unable to check disk usage for {}: {cause}", path.display())]
    Unreadable {
        /// The checked path, as resolved.
        path: PathBuf,
        /// Why not.
        cause: io::Error,
    },
}

/// A filesystem's figures, as sb-go computes them.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Usage {
    total: u64,
    free: u64,
    used_percent: f64,
    fsid: u64,
}

/// The disk checks, with the `statvfs` they read.
#[derive(Debug, Clone)]
pub struct Disk {
    statvfs: fn(&Path) -> io::Result<Stats>,
}

impl Default for Disk {
    fn default() -> Self {
        Self::new()
    }
}

impl Disk {
    /// The system's `statvfs`.
    #[must_use]
    pub fn new() -> Self {
        Self { statvfs }
    }

    /// Read filesystems with `statvfs` instead (tests;
    /// M-MOCKABLE-SYSCALLS).
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn with_statvfs(mut self, statvfs: fn(&Path) -> io::Result<Stats>) -> Self {
        self.statvfs = statvfs;
        self
    }

    /// Refuse when a filesystem holding one of `paths` has under 2 GiB
    /// free (sb-go's `CheckDiskSpace`).
    ///
    /// Each path resolves to its nearest existing ancestor, and each
    /// filesystem is checked once, by its `f_fsid`. Free space is
    /// `f_bavail × f_frsize` (sb-go used `f_bsize`; host spec, difference
    /// 10). The run log gets each filesystem's figures and each skipped
    /// duplicate.
    ///
    /// # Errors
    ///
    /// [`DiskError::Insufficient`] for the first filesystem under 2 GiB
    /// free; [`DiskError::Unreadable`] when one can't be read or reports
    /// zero blocks.
    ///
    /// `statvfs` blocks, as a hung network or FUSE mount can make it: on a
    /// runtime, call this under `spawn_blocking`.
    pub fn check_disk_space<P: AsRef<Path>>(&self, paths: &[P]) -> Result<(), DiskError> {
        let mut seen = HashSet::new();
        let mut filesystems: HashMap<u64, PathBuf> = HashMap::new();
        for path in paths {
            let path = nearest_existing(path.as_ref());
            if !seen.insert(path.clone()) {
                continue;
            }
            let usage = self.usage(&path)?;
            if let Some(first) = filesystems.get(&usage.fsid) {
                tracing::info!(
                    path = %path.display(),
                    same_as = %first.display(),
                    "disk usage skipped: same filesystem"
                );
                continue;
            }
            filesystems.insert(usage.fsid, path.clone());
            tracing::info!(
                path = %path.display(),
                total = %format_bytes(usage.total),
                free = %format_bytes(usage.free),
                used = %format!("{:.1}%", usage.used_percent),
                "disk usage"
            );
            if usage.free < MIN_FREE {
                return Err(insufficient(&path, usage.used_percent, usage.free));
            }
        }
        Ok(())
    }

    /// The free space on the filesystem holding `path` (its nearest
    /// existing ancestor): `f_bavail × f_frsize`, the figure
    /// [`Disk::check_disk_space`] compares (ops spec's amendment; bench's
    /// disk test).
    ///
    /// # Errors
    ///
    /// [`DiskError::Unreadable`] when the filesystem can't be read or
    /// reports zero blocks.
    pub fn free_space(&self, path: &Path) -> Result<u64, DiskError> {
        Ok(self.usage(&nearest_existing(path))?.free)
    }

    /// sb-go's `getDiskUsage` on an existing `path`.
    fn usage(&self, path: &Path) -> Result<Usage, DiskError> {
        let unreadable = |cause| DiskError::Unreadable {
            path: path.to_path_buf(),
            cause,
        };
        let stats = (self.statvfs)(path).map_err(unreadable)?;
        if stats.blocks == 0 {
            return Err(unreadable(io::Error::other(format!(
                "filesystem stats for {} reported zero blocks",
                path.display()
            ))));
        }
        let used = stats.blocks.saturating_sub(stats.bavail);
        Ok(Usage {
            total: stats.blocks.saturating_mul(stats.frsize),
            free: stats.bavail.saturating_mul(stats.frsize),
            used_percent: to_f64(used) / to_f64(stats.blocks) * 100.0,
            fsid: stats.fsid,
        })
    }
}

/// [`Disk::check_disk_space`] with the system's `statvfs`.
///
/// # Errors
///
/// As [`Disk::check_disk_space`].
pub fn check_disk_space<P: AsRef<Path>>(paths: &[P]) -> Result<(), DiskError> {
    Disk::new().check_disk_space(paths)
}

/// [`Disk::free_space`] with the system's `statvfs`.
///
/// # Errors
///
/// As [`Disk::free_space`].
pub fn free_space(path: &Path) -> Result<u64, DiskError> {
    Disk::new().free_space(path)
}

/// The failure for a full disk at `path`, 100% used and nothing free:
/// install's hidden `--force-disk-full` (sb-go's `DiskSpaceError`).
#[must_use]
pub fn disk_space_error(path: &Path) -> DiskError {
    insufficient(path, 100.0, 0)
}

fn insufficient(path: &Path, used: f64, free: u64) -> DiskError {
    DiskError::Insufficient {
        path: path.to_path_buf(),
        used,
        free,
    }
}

/// rustix's `statvfs`
/// (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.statvfs.html>).
fn statvfs(path: &Path) -> io::Result<Stats> {
    let stats = rustix::fs::statvfs(path)?;
    Ok(Stats {
        blocks: stats.f_blocks,
        bavail: stats.f_bavail,
        frsize: stats.f_frsize,
        fsid: stats.f_fsid,
    })
}

/// sb-go's `nearestExistingPath`: `path` itself or its nearest ancestor
/// that exists (following symlinks, as Go's `os.Stat`), or `.`
/// for a relative path none of whose ancestors does (an absolute one always
/// reaches `/`). `.` and repeated separators drop out as `filepath.Clean`
/// drops them (std's `Path::components`); a `..` stays, which a caller's
/// absolute paths never hold.
fn nearest_existing(path: &Path) -> PathBuf {
    let clean: PathBuf = path.components().collect();
    clean
        .ancestors()
        .find(|ancestor| !ancestor.as_os_str().is_empty() && ancestor.exists())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// sb-go's `formatBytes`: `N B` under 1 KiB, else one decimal in the
/// largest binary unit, up to EiB. Why not indicatif's `BinaryBytes`: it
/// prints two decimals, and sb-go's text has one.
fn format_bytes(bytes: u64) -> String {
    const UNIT: u64 = 1024;
    if bytes < UNIT {
        return format!("{bytes} B");
    }
    let (mut div, mut exp) = (UNIT, 0);
    let mut n = bytes / UNIT;
    while n >= UNIT {
        div *= UNIT;
        exp += 1;
        n /= UNIT;
    }
    let unit = ['K', 'M', 'G', 'T', 'P', 'E'][exp];
    format!("{:.1} {unit}iB", to_f64(bytes) / to_f64(div))
}

/// `value` as the nearest `f64`, as Go's `float64(uint64)` converts it:
/// both halves convert exactly, and their sum rounds once, to nearest.
/// Why this and not a library: std has no `u64` to `f64` conversion but
/// `as` (CONSTRAINTS.md's E2 records the same gap for `i64`), and
/// num-traits' `to_f64` is that `as`; `as` itself would need a
/// `cast_precision_loss` exception.
fn to_f64(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A filesystem of `total` bytes with `free` free, in 4 KiB fragments.
    fn fs(total: u64, free: u64, fsid: u64) -> Stats {
        Stats {
            blocks: total / 4096,
            bavail: free / 4096,
            frsize: 4096,
            fsid,
        }
    }

    fn check(statvfs: fn(&Path) -> io::Result<Stats>, paths: &[&str]) -> Result<(), DiskError> {
        Disk::new().with_statvfs(statvfs).check_disk_space(paths)
    }

    #[test]
    fn enough_space_passes() {
        assert!(check(|_| Ok(fs(100 * GIB, 50 * GIB, 1)), &["/"]).is_ok());
        assert!(check(|_| Ok(fs(100 * GIB, 2 * GIB, 1)), &["/"]).is_ok());
        assert!(check(|_| Ok(fs(100 * GIB, 0, 1)), &[]).is_ok());
    }

    #[test]
    fn under_two_gib_fails_with_sb_gos_text() {
        let error = check(|_| Ok(fs(100 * GIB, 2 * GIB - 4096, 1)), &["/"]).expect_err("full");
        assert_eq!(
            error.to_string(),
            "INSUFFICIENT DISK SPACE - Install cancelled: / is 98.0% full (2.0 GiB free). \
             Free up space on / before continuing"
        );
        let error = check(|_| Ok(fs(10 * GIB, GIB, 1)), &["/"]).expect_err("full");
        assert_eq!(
            error.to_string(),
            "INSUFFICIENT DISK SPACE - Install cancelled: / is 90.0% full (1.0 GiB free). \
             Free up space on / before continuing"
        );
    }

    #[test]
    fn free_space_is_bavail_times_frsize_not_bsize() {
        // 1 KiB fragments: 3 Mi of them is 3 GiB.
        let stats = |_: &Path| {
            Ok(Stats {
                blocks: 10 * 1024 * 1024,
                bavail: 3 * 1024 * 1024,
                frsize: 1024,
                fsid: 1,
            })
        };
        let disk = Disk::new().with_statvfs(stats);
        assert_eq!(disk.free_space(Path::new("/")).expect("free"), 3 * GIB);
        assert!(disk.check_disk_space(&["/"]).is_ok());
    }

    #[test]
    fn the_used_percent_is_sb_gos_formula() {
        let usage = Disk::new()
            .with_statvfs(|_| {
                Ok(Stats {
                    blocks: 3,
                    bavail: 2,
                    frsize: 1,
                    fsid: 1,
                })
            })
            .usage(Path::new("/"))
            .expect("usage");
        assert_eq!(format!("{:.1}", usage.used_percent), "33.3");
        assert_eq!(usage.total, 3);
        assert_eq!(usage.free, 2);
    }

    // The fsid, read-once and walk-up tests resolve real paths: `/` and
    // `/proc`, which every Linux host has, and names under them no host
    // has; they read no figure of the host's (statvfs is the test's).
    #[test]
    fn each_filesystem_is_checked_once_by_fsid() {
        // `/proc` shares `/`'s fsid here, so only `/` is checked: a full
        // `/proc` would otherwise fail.
        let stats = |path: &Path| {
            if path == Path::new("/") {
                Ok(fs(100 * GIB, 50 * GIB, 7))
            } else {
                Ok(fs(100 * GIB, 0, 7))
            }
        };
        assert!(check(stats, &["/", "/proc"]).is_ok());
        // Another fsid is checked.
        let stats = |path: &Path| {
            if path == Path::new("/") {
                Ok(fs(100 * GIB, 50 * GIB, 7))
            } else {
                Ok(fs(100 * GIB, 0, 8))
            }
        };
        let error = check(stats, &["/", "/proc"]).expect_err("full");
        assert!(
            error
                .to_string()
                .contains(": /proc is 100.0% full (0 B free)"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_path_is_checked_at_its_nearest_existing_ancestor() {
        let stats = |path: &Path| {
            assert_eq!(path, Path::new("/proc"));
            Ok(fs(100 * GIB, 0, 1))
        };
        let error = check(stats, &["/proc/sb-rust-missing-7fcd24d1/deeper"]).expect_err("full");
        assert!(error.to_string().contains(": /proc is "), "{error}");
    }

    #[test]
    fn the_same_resolved_path_is_read_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static READS: AtomicUsize = AtomicUsize::new(0);
        let stats = |_: &Path| {
            READS.fetch_add(1, Ordering::SeqCst);
            Ok(fs(100 * GIB, 50 * GIB, 1))
        };
        check(stats, &["/", "/sb-rust-missing-7fcd24d1", "//"]).expect("ok");
        assert_eq!(READS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_read_nests_sb_gos_text() {
        let error = check(|_| Err(io::Error::from_raw_os_error(13)), &["/"]).expect_err("failed");
        assert_eq!(
            error.to_string(),
            "unable to check disk usage for /: Permission denied (os error 13)"
        );
    }

    #[test]
    fn zero_blocks_is_sb_gos_error() {
        let error = check(|_| Ok(fs(0, 0, 1)), &["/"]).expect_err("failed");
        assert_eq!(
            error.to_string(),
            "unable to check disk usage for /: filesystem stats for / reported zero blocks"
        );
        let free = Disk::new()
            .with_statvfs(|_| Ok(fs(0, 0, 1)))
            .free_space(Path::new("/"));
        assert!(matches!(free, Err(DiskError::Unreadable { .. })));
    }

    #[test]
    fn the_forced_error_is_full_with_nothing_free() {
        assert_eq!(
            disk_space_error(Path::new("/opt")).to_string(),
            "INSUFFICIENT DISK SPACE - Install cancelled: /opt is 100.0% full (0 B free). \
             Free up space on /opt before continuing"
        );
    }

    /// The real call's smoke test, on the host's `/` on purpose: any
    /// host's has blocks and a fragment size; the figures' handling has
    /// the invented cases above.
    #[test]
    fn the_system_statvfs_reads_the_root() {
        let stats = statvfs(Path::new("/")).expect("statvfs");
        assert!(stats.blocks > 0 && stats.frsize > 0, "{stats:?}");
        let free = free_space(Path::new("/sb-rust-missing-7fcd24d1")).expect("free");
        assert!(free <= stats.blocks * stats.frsize);
        let error = Disk::new()
            .usage(Path::new("/sb-rust-missing-7fcd24d1"))
            .expect_err("missing");
        assert!(error.to_string().contains("No such file"), "{error}");
    }

    #[test]
    fn the_public_check_reads_the_system() {
        // Whatever the host's space, a check that passes or fails names `/`.
        match check_disk_space(&["/"]) {
            Ok(()) => assert!(free_space(Path::new("/")).expect("free") >= MIN_FREE),
            Err(error) => assert!(error.to_string().contains(" / is "), "{error}"),
        }
    }

    #[test]
    fn nearest_existing_walks_up_and_cleans() {
        assert_eq!(nearest_existing(Path::new("/")), Path::new("/"));
        assert_eq!(
            nearest_existing(Path::new("/proc/./sb-rust-missing-7fcd24d1/x")),
            Path::new("/proc")
        );
        assert_eq!(nearest_existing(Path::new("//proc//")), Path::new("/proc"));
        assert_eq!(
            nearest_existing(Path::new("sb-rust-missing-7fcd24d1/x")),
            Path::new(".")
        );
    }

    #[test]
    fn bytes_read_as_sb_gos() {
        let cases = [
            (0, "0 B"),
            (1023, "1023 B"),
            (1024, "1.0 KiB"),
            (1536, "1.5 KiB"),
            (1024 * 1024 - 1, "1024.0 KiB"),
            (1024 * 1024, "1.0 MiB"),
            (2 * GIB - 4096, "2.0 GiB"),
            (5 * GIB / 2, "2.5 GiB"),
            (1024 * GIB, "1.0 TiB"),
            (u64::MAX, "16.0 EiB"),
        ];
        for (bytes, text) in cases {
            assert_eq!(format_bytes(bytes), text, "{bytes}");
        }
    }

    #[test]
    fn to_f64_rounds_to_nearest() {
        for value in [
            0,
            1,
            u64::from(u32::MAX),
            1 << 32,
            (1 << 53) + 1,
            u64::MAX,
            12_345_678_901_234_567,
        ] {
            // std's parse rounds to nearest, ties to even, as a cast does.
            let cast: f64 = value.to_string().parse().expect("parse");
            assert_eq!(to_f64(value).to_bits(), cast.to_bits(), "{value}");
        }
    }
}
