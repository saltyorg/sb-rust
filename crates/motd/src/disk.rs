//! Disk Usage (motd spec, Sections and "Disk Usage, df's rules"; decision
//! 8): the mounts from /proc/self/mountinfo, each line parsed by procfs
//! (<https://docs.rs/procfs/0.18.0/procfs/process/struct.MountInfo.html#method.from_line>),
//! each file system's numbers from `statvfs(3)` and its device from
//! `stat(2)` through rustix
//! (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.statvfs.html>,
//! <https://docs.rs/rustix/1.1.6/rustix/fs/fn.stat.html>), chosen and
//! printed as `df -H -x tmpfs -x overlay -x fuse.mergerfs -x fuse.rclone
//! --output=target,pcent,size` does for sb-go (`motd/info.go:1050-1184`).
//!
//! df's rules are coreutils' and gnulib's, ported as domain logic: the
//! dummy and remote file systems (gnulib `lib/mountlist.c`, `ME_DUMMY`,
//! `ME_REMOTE`), one line per device (coreutils `src/df.c`,
//! `filter_mount_list`, v9.4, which Ubuntu 24.04 ships and 26.04's 9.7
//! keeps; 22.04's 8.32 compares a later entry with a device's first
//! entry rather than its latest, which differs only where a device has
//! three or more entries and one of them is kept beside another), the
//! skipped entries (`get_dev`), the percent (`get_field_values` and the
//! `PCENT_FIELD` case) and `-H`'s sizes (gnulib `lib/human.c`,
//! `human_readable` with `human_autoscale | human_SI`, rounding up):
//! <https://github.com/coreutils/coreutils/blob/v9.4/src/df.c>,
//! <https://github.com/coreutils/gnulib/blob/bb5bb43a/lib/mountlist.c>,
//! <https://github.com/coreutils/gnulib/blob/bb5bb43a/lib/human.c>
//! (coreutils v9.4's gnulib).
//! Why not sysinfo's `Disks`: it gives no `f_bfree`, so df's percent
//! can't be made (motd spec, Tech Stack, "Considered and not used").

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read as _;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use procfs::process::MountInfo;

use crate::packages::CHILD_TIMEOUT;
use crate::system::NOT_AVAILABLE;
use crate::text::one_line;
use crate::{Level, Paint};

/// sb-go's `-x` types: never shown, never merged with another entry.
const EXCLUDED: [&str; 4] = ["tmpfs", "overlay", "fuse.mergerfs", "fuse.rclone"];

/// gnulib's `ME_DUMMY_0` types.
const DUMMY: [&str; 13] = [
    "autofs",
    "proc",
    "subfs",
    "debugfs",
    "devpts",
    "fusectl",
    "fuse.portal",
    "mqueue",
    "rpc_pipefs",
    "sysfs",
    "devfs",
    "kernfs",
    "ignore",
];

/// The types gnulib's `ME_REMOTE` counts as remote whatever their source.
const REMOTE: [&str; 9] = [
    "acfs",
    "afs",
    "coda",
    "auristorfs",
    "fhgfs",
    "gpfs",
    "ibrix",
    "ocfs2",
    "vxfs",
];

/// The most of /proc/self/mountinfo read: far past a host's mounts.
const MOUNTINFO_CAP: u64 = 8 << 20;

/// sb-go's bar width (`barWidth`).
const BAR_WIDTH: usize = 50;

/// One mountinfo line as df reads it: escapes undone, "none" for a
/// missing source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mount {
    pub source: String,
    pub point: PathBuf,
    pub root: String,
    pub fs_type: String,
    /// Its `major:minor`, as `makedev` would make it.
    pub dev: u64,
}

/// What `statvfs` says, in fragment units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Usage {
    pub frsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
}

impl Mount {
    /// gnulib's `ME_DUMMY` for a mountinfo line (`Bind` is false there).
    fn dummy(&self) -> bool {
        DUMMY.contains(&self.fs_type.as_str()) || self.fs_type == "none"
    }

    /// gnulib's `ME_REMOTE` as coreutils 9.4 builds it.
    fn remote(&self) -> bool {
        self.source.contains(':')
            || self.source == "-hosts"
            || (self.source.starts_with("//")
                && ["smbfs", "smb3", "cifs"].contains(&self.fs_type.as_str()))
            || REMOTE.contains(&self.fs_type.as_str())
    }

    fn excluded(&self) -> bool {
        EXCLUDED.contains(&self.fs_type.as_str())
    }
}

/// A mountinfo field with the kernel's octal escapes undone, as gnulib's
/// `unescape_tab` does: `\` and three octal digits, the first 0-3.
/// Why not procfs: its `MountInfo` keeps the escapes (procfs-core 0.18.0
/// `src/process/mount.rs`, `from_line`).
fn unescape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes.get(i + 1..i + 4).filter(|digits| {
            bytes[i] == b'\\'
                && (b'0'..=b'3').contains(&digits[0])
                && (b'0'..=b'7').contains(&digits[1])
                && (b'0'..=b'7').contains(&digits[2])
        });
        if let Some(digits) = octal {
            out.push((digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + (digits[2] - b'0'));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// The mounts df reports on, in mountinfo's order: coreutils'
/// `filter_mount_list`, then `get_dev`'s skips (dummy, excluded and
/// relative entries). `device` is `stat(2)`'s `st_dev` of a mount point,
/// none when it can't be looked at. Of two entries on one device, the
/// later one replaces the earlier, in the earlier one's place, when its
/// source has a `/` and the earlier one's hasn't, when its mount point is
/// shorter and its root no longer, or when it is mounted over the same
/// point from another source; otherwise the later one is dropped. Two
/// remote entries from different sources both stay.
fn chosen(mounts: &[Mount], device: impl Fn(&Path) -> Option<u64>) -> Vec<&Mount> {
    let mut list: Vec<&Mount> = Vec::new();
    // Each device's latest entry in `list` (df 9.4's `seen_last`).
    let mut seen: HashMap<u64, usize> = HashMap::new();
    for mount in mounts {
        let looked_at = if mount.dummy() || mount.excluded() {
            None
        } else {
            device(&mount.point)
        };
        let Some(dev) = looked_at else {
            seen.insert(mount.dev, list.len());
            list.push(mount);
            continue;
        };
        if let Some(&at) = seen.get(&dev) {
            let old = list[at];
            let nearer_root = old.point.as_os_str().len() > mount.point.as_os_str().len();
            let source_below_root = old.root.len() < mount.root.len();
            if mount.remote() && old.remote() && old.source != mount.source {
                // Both kept: explicitly mounted remote exports.
            } else if (mount.source.contains('/') && !old.source.contains('/'))
                || (nearer_root && !source_below_root)
                || (old.source != mount.source && old.point == mount.point)
            {
                list[at] = mount;
                continue;
            } else {
                continue;
            }
        }
        seen.insert(dev, list.len());
        list.push(mount);
    }
    list.retain(|mount| !mount.dummy() && !mount.excluded() && mount.point.is_absolute());
    list
}

/// df's percent: used over used plus available, rounded up, where used is
/// `f_blocks - f_bfree` and available `f_bavail` (coreutils `src/df.c`,
/// `get_field_values` and `PCENT_FIELD`). None where df prints `-`,
/// which sb-go's `Atoi` then skips.
fn percent(usage: Usage) -> Option<u8> {
    let used = usage.blocks.checked_sub(usage.bfree)?;
    let total = used.checked_add(usage.bavail)?;
    if total == 0 {
        return None;
    }
    u8::try_from(used.checked_mul(100)?.div_ceil(total)).ok()
}

/// `bytes` as `df -H` prints a size: gnulib's `human_readable` with
/// `human_autoscale | human_SI` and its default rounding up, on integers
/// (its exact branch): powers of 1000, `k` then `M`, `G`, ...; one
/// decimal below 10. Why not indicatif's `DecimalBytes`, humansize or
/// bytesize: each rounds to nearest with a fixed number of decimals and a
/// `B` (indicatif 0.18.6 `src/format.rs`;
/// <https://docs.rs/humansize/2.1.3/humansize/>,
/// <https://docs.rs/bytesize/2.7.0/bytesize/>), where df rounds up and
/// shows a decimal only below 10.
fn si_size(bytes: u64) -> String {
    const LETTERS: [&str; 11] = ["", "k", "M", "G", "T", "P", "E", "Z", "Y", "R", "Q"];
    const BASE: u64 = 1000;
    let (mut amount, mut tenths, mut rounding) = (bytes, 0_u64, 0_u64);
    let mut exponent = 0;
    let mut decimal = None;
    if amount >= BASE {
        while amount >= BASE && exponent < LETTERS.len() - 1 {
            let r10 = (amount % BASE) * 10 + tenths;
            let r2 = (r10 % BASE) * 2 + (rounding >> 1);
            amount /= BASE;
            tenths = r10 / BASE;
            rounding = if r2 < BASE {
                u64::from(r2 + rounding != 0)
            } else {
                2 + u64::from(BASE < r2 + rounding)
            };
            exponent += 1;
        }
        if amount < 10 {
            if rounding > 0 {
                tenths += 1;
                rounding = 0;
                if tenths == 10 {
                    amount += 1;
                    tenths = 0;
                }
            }
            if amount < 10 {
                decimal = Some(tenths);
                tenths = 0;
                rounding = 0;
            }
        }
    }
    if tenths + rounding > 0 {
        amount += 1;
        if amount == BASE && exponent < LETTERS.len() - 1 {
            exponent += 1;
            amount = 1;
            decimal = Some(0);
        }
    }
    match decimal {
        Some(tenths) => format!("{amount}.{tenths}{}", LETTERS[exponent]),
        None => format!("{amount}{}", LETTERS[exponent]),
    }
}

/// One shown file system: its mount point made safe to draw, df's
/// percent and `-H` size.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    point: String,
    percent: u8,
    size: String,
}

/// The rows for `mounts`: df's choice, its usage from `usage` (none for
/// a file system `statvfs` can't read, which df leaves out or reports
/// as an error), no zero-size file system (df's rule without `-a`), and
/// none whose mount point starts with `/dev`, `/sys`, `/proc` or `/run`
/// (sb-go's string test on df's output).
fn rows(
    mounts: &[Mount],
    device: impl Fn(&Path) -> Option<u64>,
    usage: impl Fn(&Path) -> Option<Usage>,
) -> Vec<Row> {
    chosen(mounts, device)
        .into_iter()
        .filter_map(|mount| {
            let point = one_line(mount.point.as_os_str().as_encoded_bytes());
            if ["/dev", "/sys", "/proc", "/run"]
                .iter()
                .any(|prefix| point.starts_with(prefix))
            {
                return None;
            }
            let usage = usage(&mount.point)?;
            if usage.blocks == 0 {
                return None;
            }
            Some(Row {
                point,
                percent: percent(usage)?,
                size: si_size(usage.blocks.saturating_mul(usage.frsize)),
            })
        })
        .collect()
}

/// Disk Usage's value: sb-go's `%-30s%3d%% used out of %4s` per file
/// system, then its bar on the next line. The percent is in the value
/// colour below 80, the warning colour below 90 and the error colour
/// from 90; the bar is in `progress_bar`'s low, high or critical colour,
/// sb's Ok, Warn and Problem (motd decision 5). No row reads `No valid
/// disk partitions found`.
fn render(rows: &[Row], paint: &Paint) -> String {
    if rows.is_empty() {
        return "No valid disk partitions found".to_string();
    }
    rows.iter()
        .map(|row| {
            let shown = format!("{:>3}%", row.percent);
            let (percent, level) = match row.percent {
                0..80 => (paint.value(&shown), Level::Low),
                80..90 => (paint.warning(&shown), Level::High),
                _ => (paint.error(&shown), Level::Critical),
            };
            let size = paint.value(&format!("{:>4}", row.size));
            format!(
                "{:<30}{percent} used out of {size}\n{}",
                row.point,
                paint.bar(level, row.percent, BAR_WIDTH)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The mounts in mountinfo's `text`, as gnulib reads them: each line on
/// its own through procfs's parser, one it can't parse left out (gnulib
/// skips a line its `sscanf` doesn't match), and the escapes undone. The
/// mount point is taken from the line's bytes, as the kernel leaves bytes
/// other than whitespace and `\` raw. Why not procfs's `mountinfo()`: it
/// reads the file as UTF-8 lines and fails on the first that isn't
/// (procfs-core 0.18.0 `src/process/mount.rs`, `from_buf_read`), so one
/// such mount point would hide every disk.
fn mounts(text: &[u8]) -> Vec<Mount> {
    text.split(|&byte| byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let info = MountInfo::from_line(&String::from_utf8_lossy(line))
                .inspect_err(|err| tracing::debug!("mountinfo line skipped: {err}"))
                .ok()?;
            let raw_point = line.split(|&byte| byte == b' ').nth(4)?;
            let (major, minor) = info
                .majmin
                .split_once(':')
                .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)))
                .unwrap_or((0, 0));
            let text =
                |field: &str| String::from_utf8_lossy(&unescape(field.as_bytes())).into_owned();
            Some(Mount {
                source: text(info.mount_source.as_deref().unwrap_or("none")),
                point: PathBuf::from(OsString::from_vec(unescape(raw_point))),
                root: text(&info.root),
                fs_type: text(&info.fs_type),
                dev: rustix::fs::makedev(major, minor),
            })
        })
        .collect()
}

/// `path` read whole up to [`MOUNTINFO_CAP`] (a /proc file says its size
/// is 0, so it is read to its end).
fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MOUNTINFO_CAP)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Disk Usage on this host: mountinfo, then each file system's `stat(2)`
/// and `statvfs(3)`.
pub async fn disk_here(paint: &Paint) -> String {
    disk_within(CHILD_TIMEOUT, paint, || {
        let text = read_capped(Path::new("/proc/self/mountinfo"))?;
        Ok(rows(&mounts(&text), stat_device, statvfs_usage))
    })
    .await
}

/// `read` run on tokio's blocking pool, as `statvfs` on a hung network
/// mount blocks the thread
/// (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>), and
/// given `limit`, sb-go's 5 s for its `df` child; past it, or when
/// mountinfo can't be read, `Not available`. A panic in `read` reaches
/// the runner, which shows its error line.
async fn disk_within(
    limit: Duration,
    paint: &Paint,
    read: impl FnOnce() -> std::io::Result<Vec<Row>> + Send + 'static,
) -> String {
    // The run's span goes along (<https://docs.rs/tracing/0.1.44/tracing/struct.Span.html#method.in_scope>).
    let span = tracing::Span::current();
    let read = move || span.in_scope(read);
    match tokio::time::timeout(limit, tokio::task::spawn_blocking(read)).await {
        Ok(Ok(Ok(rows))) => render(&rows, paint),
        Ok(Ok(Err(err))) => {
            tracing::warn!("mountinfo not read: {err}");
            NOT_AVAILABLE.to_string()
        }
        Ok(Err(join)) => match join.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            Err(_) => NOT_AVAILABLE.to_string(),
        },
        Err(_) => {
            tracing::warn!("disk usage past {} ms", limit.as_millis());
            NOT_AVAILABLE.to_string()
        }
    }
}

fn stat_device(point: &Path) -> Option<u64> {
    rustix::fs::stat(point).ok().map(|stat| stat.st_dev)
}

/// gnulib's `get_fs_usage`: the fragment size, or the block size where
/// it is zero. A failure leaves the file system out; one other than
/// EACCES or ENOENT, which df would print, is logged.
fn statvfs_usage(point: &Path) -> Option<Usage> {
    let stat = rustix::fs::statvfs(point)
        .inspect_err(|err| {
            if ![rustix::io::Errno::ACCESS, rustix::io::Errno::NOENT].contains(err) {
                tracing::warn!(mount = %point.display(), "statvfs failed: {err}");
            }
        })
        .ok()?;
    Some(Usage {
        frsize: if stat.f_frsize == 0 {
            stat.f_bsize
        } else {
            stat.f_frsize
        },
        blocks: stat.f_blocks,
        bfree: stat.f_bfree,
        bavail: stat.f_bavail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_cli_core::theme::Role;

    fn fixture() -> Vec<Mount> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mountinfo");
        mounts(&std::fs::read(path).unwrap())
    }

    fn mount(source: &str, point: &str, root: &str, fs_type: &str, dev: u64) -> Mount {
        Mount {
            source: source.to_string(),
            point: PathBuf::from(point),
            root: root.to_string(),
            fs_type: fs_type.to_string(),
            dev,
        }
    }

    fn points(list: &[&Mount]) -> Vec<String> {
        list.iter().map(|m| m.point.display().to_string()).collect()
    }

    /// The fixture's `stat(2)` devices: each mount's own, but the bind
    /// mount shares /mnt/local's.
    fn device(point: &Path) -> Option<u64> {
        let dev = match point.to_str()? {
            "/dev" => rustix::fs::makedev(0, 5),
            "/" => rustix::fs::makedev(8, 2),
            "/sys/fs/cgroup" => rustix::fs::makedev(0, 28),
            "/snap/core20/2318" => rustix::fs::makedev(7, 0),
            "/boot/efi" => rustix::fs::makedev(8, 1),
            "/mnt/local" | "/mnt/media library" => rustix::fs::makedev(8, 17),
            "/mnt/nfs" => rustix::fs::makedev(0, 49),
            _ => return None,
        };
        Some(dev)
    }

    /// The fixture's `statvfs(3)` answers.
    fn usage(point: &Path) -> Option<Usage> {
        let (frsize, blocks, bfree, bavail) = match point.to_str()? {
            "/" => (4096, 5_000_000, 2_000_000, 1_750_000),
            "/sys/fs/cgroup" => (4096, 0, 0, 0),
            "/snap/core20/2318" => (131_072, 497, 0, 0),
            "/boot/efi" => (4096, 130_812, 128_000, 128_000),
            "/mnt/local" => (4096, 488_243_200, 48_824_320, 24_412_160),
            "/mnt/nfs" => (1_048_576, 100_000, 15_000, 15_000),
            _ => return None,
        };
        Some(Usage {
            frsize,
            blocks,
            bfree,
            bavail,
        })
    }

    #[test]
    fn mountinfo_is_read_as_gnulib_reads_it() {
        let mounts = fixture();
        let media = mounts
            .iter()
            .find(|m| m.root == "/media")
            .expect("the bind mount");
        assert_eq!(media.point, Path::new("/mnt/media library"));
        assert_eq!(media.source, "/dev/sdb1");
        assert_eq!(media.fs_type, "ext4");
        assert_eq!(media.dev, rustix::fs::makedev(8, 17));
        let none = mounts.iter().find(|m| m.fs_type == "none").unwrap();
        assert_eq!(none.source, "none");
        assert!(none.dummy());
    }

    /// A line procfs can't parse is left out, and a mount point of raw
    /// non-UTF-8 bytes is kept as its bytes, so its file system shows.
    #[test]
    fn one_odd_line_hides_no_other_mount() {
        let text = b"28 1 8:2 / / rw shared:1 - ext4 /dev/sda2 rw\n\
            not a mountinfo line\n\
            \n\
            80 28 8:33 / /mnt/caf\xe9 rw shared:2 - ext4 /dev/sdc1 rw\n";
        let mounts = mounts(text);
        assert_eq!(mounts.len(), 2);
        assert_eq!(
            mounts[1].point.as_os_str().as_encoded_bytes(),
            b"/mnt/caf\xe9"
        );
        assert_eq!(mounts[1].source, "/dev/sdc1");
        let usage = |_: &Path| {
            Some(Usage {
                frsize: 1000,
                blocks: 10,
                bfree: 5,
                bavail: 5,
            })
        };
        let rows = rows(&mounts, |_: &Path| None, usage);
        assert_eq!(rows[1].point, "/mnt/caf\u{FFFD}");
    }

    #[test]
    fn mountinfo_is_read_whole_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mountinfo");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(read_capped(&path).unwrap(), b"abc");
        assert!(read_capped(&dir.path().join("missing")).is_err());
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MOUNTINFO_CAP + 10).unwrap();
        assert_eq!(
            u64::try_from(read_capped(&path).unwrap().len()).unwrap(),
            MOUNTINFO_CAP
        );
    }

    #[test]
    fn the_kernels_octal_escapes_are_undone() {
        assert_eq!(unescape(br"/mnt/a\040b\011c"), b"/mnt/a b\tc");
        assert_eq!(unescape(br"back\134slash"), b"back\\slash");
        // Only `\` and three octal digits, the first 0-3.
        assert_eq!(unescape(br"\400\08x\0"), br"\400\08x\0");
        assert_eq!(unescape(br"end\04"), br"end\04");
        assert_eq!(unescape(br"\101"), b"A");
    }

    #[test]
    fn dummy_remote_and_excluded_follow_gnulib_and_sb_go() {
        let m = |source, fs_type| mount(source, "/x", "/", fs_type, 0);
        for fs_type in [
            "proc", "sysfs", "autofs", "devpts", "mqueue", "none", "ignore",
        ] {
            assert!(m("x", fs_type).dummy(), "{fs_type}");
        }
        assert!(!m("/dev/sda1", "ext4").dummy());
        for fs_type in ["tmpfs", "overlay", "fuse.mergerfs", "fuse.rclone"] {
            assert!(m("x", fs_type).excluded(), "{fs_type}");
        }
        assert!(!m("x", "devtmpfs").excluded());
        assert!(m("nas:/export", "nfs4").remote());
        assert!(m("-hosts", "autofs").remote());
        assert!(m("//nas/share", "cifs").remote());
        assert!(!m("//nas/share", "ext4").remote());
        assert!(!m("/dev/sda1", "ext4").remote());
    }

    /// The fixture's choice: dummy and excluded types gone, the bind
    /// mount folded into its device's first mount, mountinfo's order.
    #[test]
    fn df_chooses_one_entry_per_device_in_mount_order() {
        let mounts = fixture();
        assert_eq!(
            points(&chosen(&mounts, device)),
            [
                "/dev",
                "/",
                "/sys/fs/cgroup",
                "/snap/core20/2318",
                "/boot/efi",
                "/mnt/local",
                "/mnt/nfs"
            ]
        );
    }

    #[test]
    fn a_later_entry_replaces_an_earlier_one_in_its_place() {
        let dev = |_: &Path| Some(1);
        // A real device beats a pseudo name.
        let list = [
            mount("rootfs", "/a", "/", "ext4", 1),
            mount("/dev/sda1", "/b", "/", "ext4", 1),
            mount("other", "/c", "/", "ext4", 2),
        ];
        let kept = chosen(&list, |p: &Path| {
            Some(if p == Path::new("/c") { 2 } else { 1 })
        });
        assert_eq!(points(&kept), ["/b", "/c"]);
        // A shorter mount point of the same root wins.
        let list = [
            mount("/dev/sda1", "/long/path", "/", "ext4", 1),
            mount("/dev/sda1", "/srv", "/", "ext4", 1),
        ];
        assert_eq!(points(&chosen(&list, dev)), ["/srv"]);
        // ... but not one whose root is deeper.
        let list = [
            mount("/dev/sda1", "/long/path", "/", "ext4", 1),
            mount("/dev/sda1", "/srv", "/data", "ext4", 1),
        ];
        assert_eq!(points(&chosen(&list, dev)), ["/long/path"]);
        // Mounted over the same point from another source.
        let list = [
            mount("/dev/sda1", "/srv", "/", "ext4", 1),
            mount("/dev/sdb1", "/srv", "/", "ext4", 1),
        ];
        let kept = chosen(&list, dev);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].source, "/dev/sdb1");
        // A longer mount point of the same source is dropped.
        let list = [
            mount("/dev/sda1", "/srv", "/", "ext4", 1),
            mount("/dev/sda1", "/srv/again", "/", "ext4", 1),
        ];
        assert_eq!(points(&chosen(&list, dev)), ["/srv"]);
    }

    #[test]
    fn remote_exports_and_unstatable_mounts_are_all_kept() {
        let dev = |_: &Path| Some(1);
        let list = [
            mount("nas:/a", "/mnt/a", "/", "nfs4", 1),
            mount("nas:/b", "/mnt/b", "/", "nfs4", 1),
            mount("nas:/a", "/mnt/a2", "/", "nfs4", 1),
        ];
        // df 9.4 compares /mnt/a2 with the device's latest entry, /mnt/b
        // (its `seen_last`), so all three stay; 8.32 compared it with
        // /mnt/a and dropped it.
        assert_eq!(points(&chosen(&list, dev)), ["/mnt/a", "/mnt/b", "/mnt/a2"]);
        let two = [
            mount("nas:/a", "/mnt/a", "/", "nfs4", 1),
            mount("nas:/a", "/mnt/a2", "/", "nfs4", 1),
        ];
        assert_eq!(points(&chosen(&two, dev)), ["/mnt/a"]);
        let list = [
            mount("/dev/sda1", "/x", "/", "ext4", 1),
            mount("/dev/sda1", "/y", "/", "ext4", 1),
        ];
        assert_eq!(points(&chosen(&list, |_: &Path| None)), ["/x", "/y"]);
        // A relative mount point (another namespace's) is left out.
        let list = [mount("/dev/sda1", "relative", "/", "ext4", 1)];
        assert_eq!(points(&chosen(&list, dev)), [] as [&str; 0]);
    }

    /// An excluded entry holds its device number, as in df: a later
    /// entry on it is compared with it, and both can go.
    #[test]
    fn an_excluded_entry_still_holds_its_device() {
        let list = [
            mount("tmpfs", "/t", "/", "tmpfs", 9),
            mount("tmpfs", "/t/sub", "/", "ext4", 9),
        ];
        assert_eq!(points(&chosen(&list, |_: &Path| Some(9))), [] as [&str; 0]);
    }

    #[test]
    fn percent_is_dfs_rounded_up() {
        let u = |blocks, bfree, bavail| Usage {
            frsize: 4096,
            blocks,
            bfree,
            bavail,
        };
        assert_eq!(percent(u(100, 50, 50)), Some(50));
        // Reserved blocks: used over used plus available.
        assert_eq!(percent(u(100, 50, 45)), Some(53));
        assert_eq!(percent(u(1000, 999, 999)), Some(1));
        assert_eq!(percent(u(100, 100, 100)), Some(0));
        assert_eq!(percent(u(100, 0, 0)), Some(100));
        assert_eq!(percent(u(0, 0, 0)), None);
        assert_eq!(percent(u(10, 20, 20)), None);
    }

    /// Checked against coreutils' `numfmt --to=si --round=up`, the same
    /// rounding with an upper-case `K`.
    #[test]
    fn sizes_are_df_hs_si_form() {
        for (bytes, shown) in [
            (0, "0"),
            (999, "999"),
            (1000, "1.0k"),
            (1001, "1.1k"),
            (9_900, "9.9k"),
            (9_901, "10k"),
            (9_999, "10k"),
            (10_000, "10k"),
            (10_001, "11k"),
            (999_000, "999k"),
            (999_001, "1.0M"),
            (99_999_999, "100M"),
            (4_000_000_000, "4.0G"),
            (21_474_836_480, "22G"),
            (500_107_862_016, "501G"),
            (1_000_204_886_016, "1.1T"),
            (u64::MAX, "19E"),
        ] {
            assert_eq!(si_size(bytes), shown, "{bytes}");
        }
    }

    #[test]
    fn the_fixtures_rows_follow_dfs_and_sb_gos_filters() {
        let rows = rows(&fixture(), device, usage);
        let shown: Vec<(&str, u8, &str)> = rows
            .iter()
            .map(|r| (r.point.as_str(), r.percent, r.size.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("/", 64, "21G"),
                ("/snap/core20/2318", 100, "66M"),
                ("/boot/efi", 3, "536M"),
                ("/mnt/local", 95, "2.0T"),
                ("/mnt/nfs", 85, "105G"),
            ]
        );
    }

    /// sb-go's prefix test is on text: `/devices` goes too. A zero-size
    /// or unreadable file system, or one df prints `-` for, is left out.
    #[test]
    fn prefixes_zero_sizes_and_unreadable_ones_are_left_out() {
        let list = [
            mount("/dev/a", "/devices", "/", "ext4", 1),
            mount("/dev/b", "/runner", "/", "ext4", 2),
            mount("/dev/c", "/empty", "/", "ext4", 3),
            mount("/dev/d", "/gone", "/", "ext4", 4),
            mount("/dev/e", "/dash", "/", "ext4", 5),
            mount("/dev/f", "/kept", "/", "ext4", 6),
        ];
        let usage = |point: &Path| {
            let (blocks, bfree) = match point.to_str()? {
                "/empty" => (0, 0),
                "/gone" => return None,
                "/dash" => (10, 20),
                _ => (100, 50),
            };
            Some(Usage {
                frsize: 1000,
                blocks,
                bfree,
                bavail: bfree,
            })
        };
        let rows = rows(&list, |_: &Path| None, usage);
        assert_eq!(
            rows,
            [Row {
                point: "/kept".to_string(),
                percent: 50,
                size: "100k".to_string()
            }]
        );
    }

    #[test]
    fn mount_points_are_made_safe_to_draw() {
        let list = [mount("/dev/a", "/mnt/x\x1b]0;t\x07y", "/", "ext4", 1)];
        let usage = |_: &Path| {
            Some(Usage {
                frsize: 1000,
                blocks: 10,
                bfree: 5,
                bavail: 5,
            })
        };
        assert_eq!(rows(&list, |_: &Path| None, usage)[0].point, "/mnt/xy");
    }

    fn row(point: &str, percent: u8, size: &str) -> Row {
        Row {
            point: point.to_string(),
            percent,
            size: size.to_string(),
        }
    }

    /// sb-go's `%-30s%3d%% used out of %4s`, then the bar.
    #[test]
    fn each_row_is_sb_gos_line_and_its_bar() {
        let plain = Paint::plain();
        let text = render(&[row("/", 64, "21G"), row("/boot/efi", 3, "536M")], &plain);
        assert_eq!(
            text,
            format!(
                "/                              64% used out of  21G\n{}{}\n\
                 /boot/efi                       3% used out of 536M\n{}{}",
                "█".repeat(32),
                "░".repeat(18),
                "█".repeat(2),
                "░".repeat(48),
            )
        );
        let long = render(&[row(&"/x".repeat(20), 100, "2.0T")], &plain);
        assert!(long.starts_with(&format!("{}100% used out of 2.0T\n", "/x".repeat(20))));
        assert_eq!(render(&[], &plain), "No valid disk partitions found");
    }

    /// Below 80 the value and Ok colours, below 90 Warn, from 90 Problem.
    #[test]
    fn the_levels_colour_the_percent_and_the_bar() {
        let theme = sb_cli_core::theme::Theme::resolve(
            &sb_cli_core::theme::Palette::default(),
            sb_cli_core::theme::Profile::TrueColor,
        );
        let paint = Paint::new(&theme, true);
        for (percent, text, bar) in [
            (79, Role::Text, Level::Low),
            (80, Role::Warn, Level::High),
            (89, Role::Warn, Level::High),
            (90, Role::Problem, Level::Critical),
            (100, Role::Problem, Level::Critical),
        ] {
            assert_eq!(
                render(&[row("/", percent, "1.0T")], &paint),
                format!(
                    "{:<30}{} used out of {}\n{}",
                    "/",
                    sb_term_ui::style::paint(&theme, text, true, &format!("{percent:>3}%")),
                    paint.value("1.0T"),
                    paint.bar(bar, percent, BAR_WIDTH)
                ),
                "{percent}"
            );
        }
    }

    #[tokio::test]
    async fn a_slow_or_failing_read_is_not_available() {
        let plain = Paint::plain();
        let slow = disk_within(Duration::from_millis(50), &plain, || {
            std::thread::sleep(Duration::from_millis(300));
            Ok(Vec::new())
        });
        assert_eq!(slow.await, "Not available");
        let failed = disk_within(CHILD_TIMEOUT, &plain, || {
            Err(std::io::Error::other("no mountinfo"))
        });
        assert_eq!(failed.await, "Not available");
        let none = disk_within(CHILD_TIMEOUT, &plain, || Ok(Vec::new()));
        assert_eq!(none.await, "No valid disk partitions found");
    }

    #[tokio::test]
    async fn a_panic_in_the_read_reaches_the_runner() {
        let job: crate::run::Job = Box::pin(async {
            disk_within(CHILD_TIMEOUT, &Paint::plain(), || panic!("bad read")).await
        });
        let results = crate::run::collect(vec![(crate::Section::Disk, job)], crate::run::CAP).await;
        assert_eq!(results[0].1, "Error: panic occurred (bad read)");
    }

    /// The host's own mounts (read-only): a line per file system or
    /// sb-go's text for none.
    #[tokio::test]
    async fn the_hosts_disks_are_read() {
        let text = disk_here(&Paint::plain()).await;
        assert!(
            text == "No valid disk partitions found" || text.contains("% used out of "),
            "{text}"
        );
    }
}
