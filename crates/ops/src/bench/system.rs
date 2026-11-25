//! bench's System section (ops spec, "`sb bench`", 1.): each row found
//! the way sb-go's bench.sh finds it (`cmd/bench_script.sh`,
//! `get_system_info`, `check_virt` and `print_system_info`), in process
//! where the script ran a program: `/proc/cpuinfo` for the CPU rows,
//! df's sums for Total Disk, free's figures for RAM and swap, os-release
//! for the OS. The ops spec's Differences list what still differs. The
//! rules are pure functions over [`Facts`]; [`gather`] reads this host.
//! The network rows (IPv4/IPv6, Organization, Location, Region) come with
//! the network part.

use std::ffi::c_long;
use std::path::{Path, PathBuf};
use std::time::Duration;

use procfs::{CpuInfo, FromRead as _, Meminfo, MountEntry};
use sb_cli_core::cancel::CancellationToken;
use sb_cli_core::theme::Role;
use sb_exec::{Cmd, Ending};
use sb_term_ui::{Cell, Table};

use super::label;
use sysinfo::{CpuRefreshKind, RefreshKind, System};

use super::network::{self, Endpoints, Isp, Online};

/// The filesystem types bench.sh's `df -t` lists for Total Disk.
const DF_TYPES: [&str; 8] = [
    "simfs", "ext2", "ext3", "ext4", "btrfs", "xfs", "vfat", "ntfs",
];

/// The limit on each program bench runs (`systemd-detect-virt`, `zpool`;
/// the ops spec's Virtualization row).
const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

/// The limit on reading df's filesystems: a hung one blocks the thread's
/// `statvfs`. motd's 5 s for its disk row (sb-go's `df` limit).
const DISKS_TIMEOUT: Duration = Duration::from_secs(5);

/// What a row shows for a value this host didn't give: nothing, as
/// bench.sh printed an empty variable.
const BLANK: &str = "";

/// What the CPU rows show, as bench.sh reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cpu {
    /// The last `model name` (on ARM, lscpu's `Model name`).
    pub model: String,
    /// How many `processor` entries there are.
    pub cores: usize,
    /// The first `cpu MHz`, as written (on ARM, lscpu's `CPU max MHz`).
    pub mhz: Option<String>,
    /// The last `cache size`.
    pub cache: Option<String>,
    /// `aes` anywhere in the file, any case (`grep -i 'aes'`).
    pub aes: bool,
    /// `vmx` or `svm` anywhere, any case (`grep -Ei 'vmx|svm'`).
    pub virt: bool,
}

/// What bench.sh took from lscpu on ARM, in process: sysinfo's brand
/// (which names the part from `CPU implementer` and `CPU part` as lscpu
/// does) and the largest `cpuinfo_max_freq`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Arm {
    /// sysinfo's name for the CPU part (lscpu's `Model name`).
    pub brand: String,
    /// [`max_mhz`] of the CPUs' `cpuinfo_max_freq` (lscpu's `CPU max MHz`).
    pub max_mhz: Option<String>,
}

/// The CPU rows from `/proc/cpuinfo`'s `text`, parsed by procfs as
/// motd's CPU row is (procfs-core 0.18's `CpuInfo`: each value is the
/// text after the key's colon, trimmed, as bench.sh's `awk -F:` gives
/// it; <https://docs.rs/procfs-core/0.18.0/procfs_core/struct.CpuInfo.html>),
/// and searched whole for AES and virtualization as the script greps it.
/// `arm` is what lscpu gave on ARM.
#[must_use]
pub fn cpu(text: &str, arm: Option<&Arm>) -> Cpu {
    let info = CpuInfo::from_read(text.as_bytes()).ok();
    let blocks = info.as_ref().map_or(0, CpuInfo::num_cores);
    // `awk '/^processor/ {core++}'`: procfs makes one empty entry of a
    // file without any.
    let cores = text
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count();
    let field = |index: usize, name: &str| {
        info.as_ref()
            .and_then(|info| info.get_field(index, name))
            .map(str::to_owned)
    };
    let last = |name: &str| (0..blocks).rev().find_map(|index| field(index, name));
    let first_mhz = (0..blocks).find_map(|index| field(index, "cpu MHz"));
    let lower = text.to_ascii_lowercase();
    let (model, mhz) = match arm {
        Some(arm) => (arm.brand.clone(), arm.max_mhz.clone().or(first_mhz)),
        None => (last("model name").unwrap_or_default(), first_mhz),
    };
    Cpu {
        model,
        cores,
        mhz: mhz.filter(|mhz| !mhz.is_empty()),
        cache: last("cache size").filter(|cache| !cache.is_empty()),
        aes: lower.contains("aes"),
        virt: lower.contains("vmx") || lower.contains("svm"),
    }
}

/// lscpu's `CPU max MHz` from each CPU's `cpuinfo_max_freq` (kHz): the
/// largest, with lscpu's four decimals; none without cpufreq. Why not
/// sysinfo's `Cpu::frequency`: it reads `scaling_cur_freq`, the current
/// frequency (sysinfo 0.40.0 `src/unix/linux/cpu.rs`,
/// `get_cpu_frequency`); procfs has no cpufreq reader.
#[must_use]
pub fn max_mhz(khz: &[u64]) -> Option<String> {
    let max = khz.iter().copied().max()?;
    Some(format!("{}.{:03}0", max / 1000, max % 1000))
}

/// A size and how much of it is used, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// The size.
    pub total: u64,
    /// How much of it is used.
    pub used: u64,
}

impl Usage {
    fn plus(self, other: Usage) -> Usage {
        Usage {
            total: self.total.saturating_add(other.total),
            used: self.used.saturating_add(other.used),
        }
    }
}

/// One mounted filesystem as df reads it: `stat`'s `st_dev` of its mount
/// point and `statvfs`'s counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mounted {
    /// The mount point's device; none when it couldn't be looked at.
    pub dev: Option<u64>,
    /// Its type (`ext4`).
    pub file_system: String,
    /// `f_blocks`: its size in `frsize` units.
    pub blocks: u64,
    /// `f_bfree`: the free units, reserved ones included.
    pub bfree: u64,
    /// `f_frsize`, or `f_bsize` where that is 0 (gnulib's `get_fs_usage`).
    pub frsize: u64,
}

/// `df -t <bench.sh's types> --total`: each listed filesystem once by its
/// mount point's device, as df drops a device's other mounts (coreutils'
/// `filter_mount_list`; a btrfs subvolume has a device of its own, so it
/// counts again, as in df), its size `f_blocks` and its used `f_blocks -
/// f_bfree`, in `f_frsize` units. So reserved blocks aren't used.
#[must_use]
pub fn df_total(mounted: &[Mounted]) -> Usage {
    let mut seen = Vec::new();
    let mut usage = Usage::default();
    for entry in mounted {
        if !DF_TYPES.contains(&entry.file_system.as_str()) {
            continue;
        }
        if let Some(dev) = entry.dev {
            if seen.contains(&dev) {
                continue;
            }
            seen.push(dev);
        }
        usage = usage.plus(Usage {
            total: entry.blocks.saturating_mul(entry.frsize),
            used: entry
                .blocks
                .saturating_sub(entry.bfree)
                .saturating_mul(entry.frsize),
        });
    }
    usage
}

/// `zpool list -o size,allocated -Hp`'s output summed: each line's pool
/// size and allocated bytes, a line that isn't two numbers skipped. Every
/// pool counts; bench.sh passed the whole output to `calc_sum` as one
/// word, so with two pools or more it added nothing (ops spec difference
/// 46).
#[must_use]
pub fn zpool_total(stdout: &str) -> Usage {
    stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let total = fields.next()?.parse().ok()?;
            let used = fields.next()?.parse().ok()?;
            Some(Usage { total, used })
        })
        .fold(Usage::default(), Usage::plus)
}

/// Which procps `free` the release ships: its used memory differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Procps {
    /// procps 3.3, Ubuntu 22.04.
    V3,
    /// procps 4, Ubuntu 24.04 and later.
    V4,
}

/// Total RAM and swap as `free` shows them. procps 4.0.4's
/// `library/meminfo.c`: available is `MemAvailable`, or `MemFree` when
/// that is 0 or above the total; used is total - available, or total -
/// free when that is negative. procps 3.3.17's `proc/sysinfo.c`: used is
/// total - free - cached - buffers, cached being `Cached` plus
/// `SReclaimable`, or total - free when that is negative
/// (<https://gitlab.com/procps-ng/procps/-/blob/v4.0.4/library/meminfo.c>,
/// <https://gitlab.com/procps-ng/procps/-/blob/v3.3.17/proc/sysinfo.c>).
/// Swap's used is total - free in both.
#[must_use]
pub fn memory(info: &Meminfo, procps: Procps) -> (Usage, Usage) {
    let total = info.mem_total;
    let free_used = total.saturating_sub(info.mem_free);
    let used = match procps {
        Procps::V3 => {
            let cache = info.cached.saturating_add(info.s_reclaimable.unwrap_or(0));
            free_used
                .checked_sub(info.buffers)
                .and_then(|left| left.checked_sub(cache))
                .unwrap_or(free_used)
        }
        Procps::V4 => {
            let available = info
                .mem_available
                .filter(|&available| available != 0 && available <= total)
                .unwrap_or(info.mem_free);
            total.checked_sub(available).unwrap_or(free_used)
        }
    };
    let swap = Usage {
        total: info.swap_total,
        used: info.swap_total.saturating_sub(info.swap_free),
    };
    (Usage { total, used }, swap)
}

/// `key`'s value in an os-release style file as bench.sh's `awk -F=`
/// takes it: the first `key=` line's text up to any next `=`, a double
/// quote at either end removed. Why not sysinfo: it has no `PRETTY_NAME`
/// or `DISTRIB_DESCRIPTION` (its `long_os_version` is `Linux (Ubuntu
/// 24.04)`), and it removes every `"`; why not motd's parser: it is
/// motd's own and follows the os-release syntax, not the script's awk.
/// `VERSION_ID` is read here too, from the same file the OS row reads
/// (host's `ubuntu_release` reads it through sysinfo).
#[must_use]
pub fn release_value(text: &str, key: &str) -> Option<String> {
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))?;
    let value = value.split('=').next().unwrap_or_default();
    let value = value.strip_prefix('"').unwrap_or(value);
    Some(value.strip_suffix('"').unwrap_or(value).to_owned())
}

/// The OS row, as bench.sh's `get_opsy`: os-release's `PRETTY_NAME`, else
/// lsb-release's `DISTRIB_DESCRIPTION`, else redhat-release's text, each
/// file asked only when the one before it is missing.
#[must_use]
pub fn os(
    os_release: Option<&str>,
    lsb_release: Option<&str>,
    redhat_release: Option<&str>,
) -> Option<String> {
    match (os_release, lsb_release) {
        (Some(text), _) => release_value(text, "PRETTY_NAME"),
        (None, Some(text)) => release_value(text, "DISTRIB_DESCRIPTION"),
        (None, None) => redhat_release.map(|text| text.trim_end().to_owned()),
    }
}

/// `bytes` in binary units, `KiB` to `TiB`, one decimal, and `0 KiB` for
/// nothing, as bench.sh's `calc_size` (its units are binary; sb names
/// them so, ops spec difference 15). Why not indicatif's `BinaryBytes` or
/// host's `format_bytes`: they start at bytes and print two decimals or
/// go up to EiB.
#[must_use]
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes == 0 {
        return "0 KiB".to_owned();
    }
    // Tenths of the unit, rounded, in integers: no lossy `as` cast.
    let mut divisor: u128 = 1024;
    let mut unit = 0;
    while unit + 1 < UNITS.len() && u128::from(bytes) >= divisor * 1024 {
        divisor *= 1024;
        unit += 1;
    }
    let tenths = (u128::from(bytes) * 10 + divisor / 2) / divisor;
    format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
}

/// `<size> (<used> Used)`.
fn usage(usage: Usage) -> String {
    format!("{} ({} Used)", size(usage.total), size(usage.used))
}

/// `count` and its unit, plural unless it is one.
fn counted(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit}")
    } else {
        format!("{count} {unit}s")
    }
}

/// The uptime as bench.sh's `awk` splits it (whole days, hours, minutes),
/// each unit singular or plural as its number needs (ops spec difference:
/// the script printed `days`, `hour` and `min` whatever the number).
#[must_use]
pub fn uptime(seconds: u64) -> String {
    format!(
        "{}, {}, {}",
        counted(seconds / 86_400, "day"),
        counted(seconds % 86_400 / 3_600, "hour"),
        counted(seconds % 3_600 / 60, "min")
    )
}

/// The load averages as `w` prints them: `<1>, <5>, <15>`, two decimals.
#[must_use]
pub fn load(one: f64, five: f64, fifteen: f64) -> String {
    format!("{one:.2}, {five:.2}, {fifteen:.2}")
}

/// The Virtualization row from `systemd-detect-virt`'s stdout, as
/// bench.sh reads it: an answer other than `none`, upper-cased, whatever
/// the exit code; anything else is `Dedicated`, the script's last word
/// (its other detectors go: ops spec difference 16).
#[must_use]
pub fn virtualization(stdout: &str) -> String {
    let answer = stdout.trim();
    if answer.is_empty() || answer == "none" {
        "Dedicated".to_owned()
    } else {
        answer.to_uppercase()
    }
}

/// Everything the System rows show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    pub cpu: Cpu,
    /// Total Disk: df's total, the swap and the zfs pools; none when the
    /// mounts couldn't be listed in time.
    pub disk: Option<Usage>,
    pub ram: Usage,
    pub swap: Usage,
    /// Seconds since boot.
    pub uptime: Option<u64>,
    /// The 1, 5 and 15 minute load averages.
    pub load: Option<(f64, f64, f64)>,
    pub os: Option<String>,
    /// `uname -m`.
    pub arch: String,
    pub kernel: Option<String>,
    pub congestion: Option<String>,
    pub virtualization: String,
    /// icanhazip on each family, and ipinfo's fields.
    pub online: Online,
    pub isp: Isp,
    /// The network couldn't be checked: the system's CA certificates are
    /// missing (ops spec, decision 44).
    pub ca_missing: bool,
}

/// A two-column row's cells.
fn row(text: &str, value: impl Into<String>) -> [Cell; 2] {
    [label(text), Cell::new(value)]
}

/// `✓ Enabled` in the Ok role, or `✗ Disabled` in the Problem role.
fn enabled(on: bool) -> Cell {
    if on {
        Cell::new("✓ Enabled").role(Role::Ok)
    } else {
        Cell::new("✗ Disabled").role(Role::Problem)
    }
}

/// The System section's rows (bench's ruled layout): bench.sh's rows in
/// its order, CPU Cache left out without a cache size and Total Swap
/// without swap, as the script did.
#[must_use]
pub fn table(facts: &Facts) -> Table {
    table_with(facts, None)
}

/// The section while the network is asked (ops spec, "`sb bench`",
/// Progress): the host's rows as [`table`] shows them, then one
/// `IPv4/IPv6` row with `pending` as its value in place of the network's
/// rows.
#[must_use]
pub fn live_table(facts: &Facts, pending: Cell) -> Table {
    table_with(facts, Some(pending))
}

/// The System rows, the network's or the one pending row.
fn table_with(facts: &Facts, pending: Option<Cell>) -> Table {
    let mut table = super::rows();
    let model = if facts.cpu.model.is_empty() {
        "CPU model not detected"
    } else {
        facts.cpu.model.as_str()
    };
    table.row(row("CPU Model", model));
    // awk's count prints nothing for no `processor` line.
    let count = match facts.cpu.cores {
        0 => String::new(),
        n => n.to_string(),
    };
    let cores = match &facts.cpu.mhz {
        Some(mhz) => format!("{count} @ {mhz} MHz"),
        None => count,
    };
    table.row(row("CPU Cores", cores));
    if let Some(cache) = &facts.cpu.cache {
        table.row(row("CPU Cache", cache.as_str()));
    }
    table.row([label("AES-NI"), enabled(facts.cpu.aes)]);
    table.row([label("VM-x/AMD-V"), enabled(facts.cpu.virt)]);
    table.row(row(
        "Total Disk",
        facts.disk.map_or_else(|| BLANK.to_owned(), usage),
    ));
    table.row(row("Total RAM", usage(facts.ram)));
    if facts.swap.total > 0 {
        table.row(row("Total Swap", usage(facts.swap)));
    }
    let shown = |value: Option<String>| value.unwrap_or_else(|| BLANK.to_owned());
    table.row(row("System Uptime", shown(facts.uptime.map(uptime))));
    table.row(row(
        "Load Average",
        shown(
            facts
                .load
                .map(|(one, five, fifteen)| load(one, five, fifteen)),
        ),
    ));
    table.row(row("OS", shown(facts.os.clone())));
    // `getconf LONG_BIT`: the C `long` sb was built with.
    table.row(row(
        "Arch",
        format!("{} ({} Bit)", facts.arch, c_long::BITS),
    ));
    table.row(row("Kernel", shown(facts.kernel.clone())));
    table.row(row("TCP Congestion Ctrl", shown(facts.congestion.clone())));
    table.row(row("Virtualization", facts.virtualization.as_str()));
    if let Some(pending) = pending {
        table.row([label("IPv4/IPv6"), pending]);
    } else if facts.ca_missing {
        table.row(network::missing_ca_row());
    } else {
        for row in network::rows(facts.online, &facts.isp) {
            table.row(row);
        }
    }
    table
}

/// Where [`gather`] reads this host.
#[derive(Debug, Clone)]
pub struct Sources {
    /// `/proc/cpuinfo`.
    pub cpuinfo: PathBuf,
    /// The directory of `cpu<n>/cpufreq/cpuinfo_max_freq` (ARM).
    pub cpus: PathBuf,
    /// `/proc/meminfo`.
    pub meminfo: PathBuf,
    /// The OS row's files, in bench.sh's order.
    pub os_release: PathBuf,
    pub lsb_release: PathBuf,
    pub redhat_release: PathBuf,
    /// `net.ipv4.tcp_congestion_control`'s file.
    pub congestion: PathBuf,
    /// The programs run: `systemd-detect-virt` and `zpool`.
    pub detect_virt: PathBuf,
    pub zpool: PathBuf,
    /// Lists df's filesystems, within `disks_limit`; none when the
    /// mounts can't be read.
    pub disks: fn() -> Option<Vec<Mounted>>,
    /// How long `disks` may take.
    pub disks_limit: Duration,
    /// The machine (sysinfo's `cpu_arch`, uname's), which decides how
    /// the CPU rows are read: on ARM, lscpu's way.
    pub arch: fn() -> String,
    /// On ARM, the CPU part's name as lscpu gives it ([`Arm::brand`]).
    pub arm_brand: fn() -> String,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            cpuinfo: PathBuf::from("/proc/cpuinfo"),
            cpus: PathBuf::from("/sys/devices/system/cpu"),
            meminfo: PathBuf::from("/proc/meminfo"),
            os_release: PathBuf::from("/etc/os-release"),
            lsb_release: PathBuf::from("/etc/lsb-release"),
            redhat_release: PathBuf::from("/etc/redhat-release"),
            congestion: PathBuf::from("/proc/sys/net/ipv4/tcp_congestion_control"),
            detect_virt: PathBuf::from("systemd-detect-virt"),
            zpool: PathBuf::from("zpool"),
            disks: mounted_disks,
            disks_limit: DISKS_TIMEOUT,
            arch: System::cpu_arch,
            arm_brand: sysinfo_brand,
        }
    }
}

/// sysinfo's name for this host's CPU part, which it takes from
/// cpuinfo's `CPU implementer` and `CPU part` as lscpu does; empty when
/// sysinfo can't read the CPUs.
/// <https://docs.rs/sysinfo/0.40.0/sysinfo/struct.Cpu.html#method.brand>
fn sysinfo_brand() -> String {
    System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()))
        .inspect_err(|err| tracing::warn!(what = "CPU brand", error = %err, "value not read"))
        .ok()
        .as_ref()
        .and_then(|system| system.cpus().first())
        .map_or_default(|cpu| cpu.brand().to_owned())
}

/// This host's [`Facts`]: [`gather_local`], then [`with_network`].
///
/// # Errors
///
/// As [`gather_local`] and [`with_network`].
pub async fn gather(
    sources: &Sources,
    endpoints: Option<&Endpoints>,
    token: &CancellationToken,
) -> anyhow::Result<Facts> {
    with_network(gather_local(sources, token).await?, endpoints, token).await
}

/// This host's [`Facts`] but the network's, which bench shows at once
/// (ops spec, decision 47): the files and sysinfo on the blocking pool;
/// the mounts there within `disks_limit` (cli-core's `spawn::blocking`,
/// which keeps the run's span); then `zpool` and `systemd-detect-virt`
/// through exec within 5 s each. A value that can't be read is logged and
/// shown blank.
///
/// # Errors
///
/// When `/proc/meminfo` can't be read, `token` was cancelled
/// (`interrupted`; a hung mount's thread is left behind, as motd's), or a
/// blocking task panicked.
pub async fn gather_local(sources: &Sources, token: &CancellationToken) -> anyhow::Result<Facts> {
    let files = sources.clone();
    let mut facts = sb_cli_core::spawn::blocking(move || read_host(&files)).await??;
    let df = df_usage(sources.disks, sources.disks_limit, token).await;
    let pools = zpools(&sources.zpool, token).await;
    facts.disk = df.map(|df| df.plus(facts.swap).plus(pools));
    facts.virtualization = detect_virt(&sources.detect_virt, token).await;
    if token.is_cancelled() {
        anyhow::bail!("interrupted");
    }
    Ok(facts)
}

/// `facts` with the network's rows: icanhazip and ipinfo at `endpoints`
/// at once (none: no request, offline and no ISP), or the missing CA
/// bundle when no client could be built.
///
/// # Errors
///
/// When `token` was cancelled (`interrupted`).
pub async fn with_network(
    mut facts: Facts,
    endpoints: Option<&Endpoints>,
    token: &CancellationToken,
) -> anyhow::Result<Facts> {
    if let Some(endpoints) = endpoints {
        match tokio::join!(
            network::online(endpoints, token),
            network::isp(endpoints, token)
        ) {
            (Ok(online), Ok(isp)) => (facts.online, facts.isp) = (online, isp),
            _ => facts.ca_missing = true,
        }
    }
    if token.is_cancelled() {
        anyhow::bail!("interrupted");
    }
    Ok(facts)
}

/// df's total from `list` on tokio's blocking pool, given `limit` and
/// stopped by `token`, as motd's `disk_within`
/// (<https://docs.rs/tokio/1.54.1/tokio/time/fn.timeout.html>,
/// <https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.run_until_cancelled>);
/// none past the limit or when cancelled. A panic in `list` goes on up.
async fn df_usage(
    list: fn() -> Option<Vec<Mounted>>,
    limit: Duration,
    token: &CancellationToken,
) -> Option<Usage> {
    let listed = token
        .run_until_cancelled(tokio::time::timeout(
            limit,
            sb_cli_core::spawn::blocking(list),
        ))
        .await?;
    match listed {
        Ok(Ok(mounted)) => mounted.map(|mounted| df_total(&mounted)),
        Ok(Err(join)) => match join.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            Err(_) => None,
        },
        Err(_) => {
            tracing::warn!(limit_ms = limit.as_millis(), "disks not listed in time");
            None
        }
    }
}

/// The mounts of df's listed types on this host (`/proc/self/mounts`).
fn mounted_disks() -> Option<Vec<Mounted>> {
    df_mounts(Path::new("/proc/self/mounts"))
}

/// The mounts of df's listed types in the mounts file `path`, with their
/// device and `statvfs` counts, as df's `filter_mount_list` reads them:
/// only a listed type is looked at, so no FUSE (rclone, mergerfs) or
/// overlay mount gets a `stat` or `statvfs`. The list is procfs's
/// (<https://docs.rs/procfs-core/0.18.0/procfs_core/struct.MountEntry.html>),
/// `\040` undone too, which procfs leaves (procfs-core 0.18.0
/// `unmangle_octal` has tab, newline, backslash and `#`); the device is
/// rustix's `stat` of the mount point, none when it fails (df then keeps
/// the entry), the counts rustix's `statvfs`
/// (<https://docs.rs/rustix/1.1.6/rustix/fs/fn.statvfs.html>). Why not
/// sysinfo's `Disks`: it `stat`s every mount it lists, FUSE ones
/// included, leaves out mounts under `/run`, and its storage refresh
/// gives available, not `f_bfree`. A filesystem `statvfs` fails on is
/// left out, as df leaves it out of its total; a file that can't be read
/// (procfs fails on a line that isn't UTF-8) is none, so Total Disk is
/// blank rather than short. (A mount point holding a literal `\040` is
/// read as a space, procfs having undone `\134` first; its `statvfs`
/// then fails and it is left out.)
fn df_mounts(path: &Path) -> Option<Vec<Mounted>> {
    let entries = Vec::<MountEntry>::from_file(path)
        .inspect_err(|err| tracing::warn!(error = %err, "mounts not listed"))
        .ok()?;
    let mounted = entries
        .into_iter()
        .filter(|entry| DF_TYPES.contains(&entry.fs_vfstype.as_str()))
        .filter_map(|entry| {
            let point = PathBuf::from(entry.fs_file.replace("\\040", " "));
            let stat = rustix::fs::statvfs(&point)
                .inspect_err(|err| {
                    tracing::warn!(mount = %point.display(), error = %err, "statvfs failed");
                })
                .ok()?;
            Some(Mounted {
                dev: rustix::fs::stat(&point).ok().map(|stat| stat.st_dev),
                file_system: entry.fs_vfstype,
                blocks: stat.f_blocks,
                bfree: stat.f_bfree,
                frsize: if stat.f_frsize == 0 {
                    stat.f_bsize
                } else {
                    stat.f_frsize
                },
            })
        })
        .collect();
    Some(mounted)
}

/// The zfs pools' size and allocated bytes from `zpool list`, as bench.sh
/// added them to Total Disk; nothing without zpool or pools.
async fn zpools(program: &Path, token: &CancellationToken) -> Usage {
    let ran = Cmd::new(program.as_os_str())
        .arg("list")
        .arg("-o")
        .arg("size,allocated")
        .arg("-Hp")
        .timeout(CHILD_TIMEOUT)
        .capture(token)
        .await;
    match ran {
        Ok(ran) if ran.outcome.ending == Ending::Exited(0) => zpool_total(&ran.stdout),
        Ok(ran) => {
            tracing::info!(ending = %ran.outcome.ending, "zpool list failed");
            Usage::default()
        }
        Err(err) => {
            tracing::info!(error = %err, cause = %err.cause(), "zpool didn't start");
            Usage::default()
        }
    }
}

/// The files and sysinfo's values.
/// sysinfo's calls: <https://docs.rs/sysinfo/0.40.0/sysinfo/struct.System.html>.
fn read_host(sources: &Sources) -> anyhow::Result<Facts> {
    let logged = |what: &'static str| move |err: &dyn std::fmt::Display| tracing::warn!(what, error = %err, "value not read");
    let arch = (sources.arch)();
    let arm = (arch.starts_with("aarch64") || arch.starts_with("arm")).then(|| Arm {
        brand: (sources.arm_brand)(),
        max_mhz: max_mhz(&max_freqs(&sources.cpus)),
    });
    let cpuinfo = read_file(&sources.cpuinfo, logged("cpuinfo")).unwrap_or_default();
    let os_release = read_file(&sources.os_release, logged("os-release"));
    let procps = os_release
        .as_deref()
        .and_then(|text| release_value(text, "VERSION_ID"))
        .is_some_and(|version| version == "22.04");
    let procps = if procps { Procps::V3 } else { Procps::V4 };
    let meminfo = Meminfo::from_file(&sources.meminfo)
        .map_err(|err| anyhow::anyhow!("reading {}: {err}", sources.meminfo.display()))?;
    let (ram, swap) = memory(&meminfo, procps);
    Ok(Facts {
        cpu: cpu(&cpuinfo, arm.as_ref()),
        disk: None,
        ram,
        swap,
        uptime: System::uptime()
            .inspect_err(|err| logged("uptime")(err))
            .ok(),
        load: System::load_average()
            .inspect_err(|err| logged("load average")(err))
            .ok()
            .map(|avg| (avg.one, avg.five, avg.fifteen)),
        os: os(
            os_release.as_deref(),
            read_file(&sources.lsb_release, logged("lsb-release")).as_deref(),
            read_file(&sources.redhat_release, logged("redhat-release")).as_deref(),
        ),
        arch,
        kernel: System::kernel_version()
            .inspect_err(|err| logged("kernel version")(err))
            .ok(),
        congestion: read_file(&sources.congestion, logged("congestion control"))
            .map(|text| text.trim().to_owned()),
        virtualization: String::new(),
        online: Online::default(),
        isp: Isp::default(),
        ca_missing: false,
    })
}

/// Each `cpu<n>/cpufreq/cpuinfo_max_freq` under `dir`, in kHz.
fn max_freqs(dir: &Path) -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("cpu"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|entry| {
            std::fs::read_to_string(entry.path().join("cpufreq/cpuinfo_max_freq"))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
        .collect()
}

/// A file's text, or none with the error logged.
fn read_file(path: &Path, log: impl Fn(&dyn std::fmt::Display)) -> Option<String> {
    std::fs::read_to_string(path)
        .inspect_err(|err| log(err))
        .ok()
}

/// The Virtualization row: `systemd-detect-virt` within 5 s, stopped by
/// `token`; its stderr is ignored, as the script's `2>/dev/null`.
async fn detect_virt(program: &Path, token: &CancellationToken) -> String {
    match Cmd::new(program.as_os_str())
        .timeout(CHILD_TIMEOUT)
        .capture(token)
        .await
    {
        Ok(ran) => virtualization(&ran.stdout),
        Err(err) => {
            tracing::warn!(error = %err, cause = %err.cause(), "systemd-detect-virt didn't start");
            virtualization(BLANK)
        }
    }
}

#[cfg(test)]
mod tests {
    use sb_cli_core::theme::Theme;

    use super::*;

    /// An `x86_64` `/proc/cpuinfo` of two processors (synthetic, in the
    /// kernel's layout), with the `vmx flags` line newer kernels add.
    const X86_64: &str = "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: \
                          Example CPU @ 2.40GHz\ncpu MHz\t\t: 2400.000\ncache size\t: 32768 KB\n\
                          flags\t\t: fpu vme de pse sse2 aes avx vmx\n\
                          vmx flags\t: vnmi preemption_timer\n\nprocessor\t: 1\n\
                          model name\t: Example CPU @ 2.40GHz (later)\ncpu MHz\t\t: 1200.000\n\
                          cache size\t: 1024 KB\nflags\t\t: fpu\n";

    /// An `aarch64` `/proc/cpuinfo` (synthetic): `Features`, no model
    /// name, cache size or frequency.
    const AARCH64: &str = "processor\t: 0\nBogoMIPS\t: 50.00\nFeatures\t: fp asimd evtstrm \
                           aes pmull sha1 sha2 crc32\nCPU implementer\t: 0x41\n\n\
                           processor\t: 1\nBogoMIPS\t: 50.00\nFeatures\t: fp asimd\n";

    #[test]
    fn x86_64_takes_the_last_model_and_cache_and_the_first_frequency() {
        assert_eq!(
            cpu(X86_64, None),
            Cpu {
                model: "Example CPU @ 2.40GHz (later)".to_owned(),
                cores: 2,
                mhz: Some("2400.000".to_owned()),
                cache: Some("1024 KB".to_owned()),
                aes: true,
                virt: true,
            }
        );
    }

    #[test]
    fn aes_and_virtualization_are_found_anywhere_in_any_case() {
        // As `grep -i`: part of a word counts too.
        let info = cpu("processor\t: 0\nflags\t\t: fpu SVM vaes\n", None);
        assert_eq!((info.aes, info.virt), (true, true));
        let info = cpu("processor\t: 0\nflags\t\t: fpu sse\n", None);
        assert_eq!((info.aes, info.virt), (false, false));
        let info = cpu("processor\t: 0\nflags\t\t: fpu\nvmx flags\t: ept\n", None);
        assert!(info.virt);
    }

    #[test]
    fn arm_takes_lscpus_model_and_maximum_frequency() {
        let arm = Arm {
            brand: "Neoverse-N1".to_owned(),
            max_mhz: Some("3000.0000".to_owned()),
        };
        assert_eq!(
            cpu(AARCH64, Some(&arm)),
            Cpu {
                model: "Neoverse-N1".to_owned(),
                cores: 2,
                mhz: Some("3000.0000".to_owned()),
                cache: None,
                aes: true,
                virt: false,
            }
        );
        // No cpufreq: cpuinfo's frequency, here none.
        let arm = Arm {
            max_mhz: None,
            ..arm
        };
        assert_eq!(cpu(AARCH64, Some(&arm)).mhz, None);
        let with_mhz = format!("{AARCH64}cpu MHz\t: 1000.000\n");
        assert_eq!(cpu(&with_mhz, Some(&arm)).mhz.as_deref(), Some("1000.000"));
    }

    #[test]
    fn nothing_readable_gives_no_cpu_values() {
        assert_eq!(cpu("", None), Cpu::default());
        assert_eq!(
            cpu("processor\t: 0\ncache size\t:   \ncpu MHz\t:\n", None).cache,
            None
        );
        assert_eq!(cpu("processor\t: 0\ncpu MHz\t:\n", None).mhz, None);
    }

    #[test]
    fn the_maximum_frequency_has_lscpus_four_decimals() {
        assert_eq!(max_mhz(&[]), None);
        assert_eq!(
            max_mhz(&[2_000_000, 3_000_500, 1_000_000]).as_deref(),
            Some("3000.5000")
        );
        assert_eq!(max_mhz(&[999]).as_deref(), Some("0.9990"));
    }

    #[test]
    fn max_freqs_reads_each_cpus_cpufreq() {
        let dir = tempfile::tempdir().expect("dir");
        for (name, khz) in [
            ("cpu0", "2000000\n"),
            ("cpu1", "3000000\n"),
            ("cpufreq", "1"),
        ] {
            let freq = dir.path().join(name).join("cpufreq");
            std::fs::create_dir_all(&freq).expect("dir");
            std::fs::write(freq.join("cpuinfo_max_freq"), khz).expect("write");
        }
        std::fs::create_dir_all(dir.path().join("cpu2")).expect("no cpufreq");
        let mut freqs = max_freqs(dir.path());
        freqs.sort_unstable();
        assert_eq!(freqs, [2_000_000, 3_000_000]);
        assert_eq!(max_freqs(&dir.path().join("none")), Vec::<u64>::new());
    }

    fn mounted(dev: Option<u64>, fs: &str, blocks: u64, bfree: u64) -> Mounted {
        Mounted {
            dev,
            file_system: fs.to_owned(),
            blocks,
            bfree,
            frsize: 4096,
        }
    }

    #[test]
    fn df_counts_each_device_once_and_reserved_blocks_as_free() {
        let list = [
            mounted(Some(1), "ext4", 100, 40),
            // A bind mount of the same filesystem.
            mounted(Some(1), "ext4", 100, 40),
            mounted(Some(2), "xfs", 50, 50),
            // Its device couldn't be looked at: counted, as df keeps it.
            mounted(None, "btrfs", 10, 0),
            mounted(None, "btrfs", 10, 0),
            // Not df's types: zfs (zpool's), a tmpfs, an overlay, FUSE.
            mounted(Some(3), "zfs", 7, 0),
            mounted(Some(4), "tmpfs", 7, 0),
            mounted(Some(5), "overlay", 7, 0),
            mounted(Some(6), "fuse.rclone", 7, 0),
        ];
        assert_eq!(
            df_total(&list),
            Usage {
                total: 170 * 4096,
                used: 80 * 4096,
            }
        );
    }

    #[test]
    fn every_df_type_counts() {
        for fs in DF_TYPES {
            assert_eq!(
                df_total(&[mounted(Some(1), fs, 8, 2)]).used,
                6 * 4096,
                "{fs}"
            );
        }
        assert_eq!(df_total(&[]), Usage::default());
    }

    #[test]
    fn df_mounts_reads_only_dfs_types_with_their_counts() {
        let dir = tempfile::tempdir().expect("dir");
        let spaced = dir.path().join("with space");
        std::fs::create_dir(&spaced).expect("dir");
        let mounts = dir.path().join("mounts");
        std::fs::write(
            &mounts,
            format!(
                "/dev/sda1 {} ext4 rw,relatime 0 0\n\
                 /dev/sda2 {} xfs rw 0 0\n\
                 rclone {} fuse.rclone rw 0 0\n\
                 /dev/sda3 {} ext4 rw 0 0\n",
                dir.path().display(),
                spaced.display().to_string().replace(' ', "\\040"),
                dir.path().join("fuse").display(),
                dir.path().join("gone").display(),
            ),
        )
        .expect("mounts");
        let listed = df_mounts(&mounts).expect("listed");
        // The missing mount point's statvfs fails: left out, as df does.
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert_eq!(listed[0].file_system, "ext4");
        assert_eq!(listed[1].file_system, "xfs");
        // Both directories are on one filesystem: one device, real counts.
        assert!(listed[0].dev.is_some() && listed[0].dev == listed[1].dev);
        assert!(listed[0].blocks > 0 && listed[0].frsize > 0);
        assert_eq!(df_total(&listed).total, listed[0].blocks * listed[0].frsize);
        assert_eq!(df_mounts(&dir.path().join("none")), None);
    }

    #[test]
    fn zpool_sums_each_pools_size_and_allocation() {
        assert_eq!(
            zpool_total("1000\t400\n50\t5\nno pools available\n\n"),
            Usage {
                total: 1050,
                used: 405,
            }
        );
        assert_eq!(zpool_total(""), Usage::default());
        assert_eq!(zpool_total("10\n"), Usage::default());
    }

    fn meminfo(text: &str) -> Meminfo {
        Meminfo::from_read(text.as_bytes()).expect("meminfo")
    }

    /// The fields procfs needs, in kB.
    const MEMINFO: &str = "MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 600 kB\n\
                           Buffers: 50 kB\nCached: 300 kB\nSwapCached: 0 kB\nActive: 0 kB\n\
                           Inactive: 0 kB\nUnevictable: 0 kB\nMlocked: 0 kB\n\
                           SwapTotal: 200 kB\nSwapFree: 150 kB\nDirty: 0 kB\nWriteback: 0 kB\n\
                           AnonPages: 0 kB\nMapped: 0 kB\nShmem: 0 kB\nSlab: 0 kB\n\
                           SReclaimable: 20 kB\nSUnreclaim: 0 kB\nKernelStack: 0 kB\n\
                           PageTables: 0 kB\nNFS_Unstable: 0 kB\nBounce: 0 kB\n\
                           WritebackTmp: 0 kB\nCommitLimit: 0 kB\nCommitted_AS: 0 kB\n\
                           VmallocTotal: 0 kB\nVmallocUsed: 0 kB\nVmallocChunk: 0 kB\n";

    #[test]
    fn memory_is_free_s_for_each_procps() {
        let info = meminfo(MEMINFO);
        let swap = Usage {
            total: 200 * 1024,
            used: 50 * 1024,
        };
        // procps 4: total - available.
        assert_eq!(
            memory(&info, Procps::V4),
            (
                Usage {
                    total: 1000 * 1024,
                    used: 400 * 1024,
                },
                swap
            )
        );
        // procps 3.3: total - free - buffers - (cached + reclaimable).
        assert_eq!(memory(&info, Procps::V3).0.used, 530 * 1024);
        assert_eq!(memory(&info, Procps::V3).1, swap);
        // Negative: total - free.
        let info = meminfo(&MEMINFO.replace("Cached: 300 kB", "Cached: 900 kB"));
        assert_eq!(memory(&info, Procps::V3).0.used, 900 * 1024);
        // procps 4: MemAvailable above the total, or 0, is MemFree.
        for available in ["1200", "0"] {
            let info = meminfo(
                &MEMINFO.replace("MemAvailable: 600", &format!("MemAvailable: {available}")),
            );
            assert_eq!(memory(&info, Procps::V4).0.used, 900 * 1024, "{available}");
        }
    }

    #[test]
    fn the_os_is_pretty_name_else_lsb_description() {
        let os_release =
            "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nVERSION_ID=\"24.04\"\n";
        assert_eq!(
            os(
                Some(os_release),
                Some("DISTRIB_DESCRIPTION=\"Other\""),
                None
            )
            .as_deref(),
            Some("Ubuntu 24.04.1 LTS")
        );
        assert_eq!(
            os(
                None,
                Some("DISTRIB_ID=Ubuntu\nDISTRIB_DESCRIPTION=\"Ubuntu 22.04.4 LTS\"\n"),
                Some("Other")
            )
            .as_deref(),
            Some("Ubuntu 22.04.4 LTS")
        );
        // A file found decides, even without the key.
        assert_eq!(
            os(Some("NAME=x\n"), Some("DISTRIB_DESCRIPTION=y"), None),
            None
        );
        assert_eq!(
            os(None, None, Some("Rocky Linux release 9.4 (Blue Onyx)\n")).as_deref(),
            Some("Rocky Linux release 9.4 (Blue Onyx)")
        );
        assert_eq!(os(None, None, None), None);
        assert_eq!(
            release_value(os_release, "VERSION_ID").as_deref(),
            Some("24.04")
        );
        // awk -F= takes the text up to the next `=`; unquoted stays as is.
        assert_eq!(release_value("A=b=c\n", "A").as_deref(), Some("b"));
        assert_eq!(release_value("A=plain\n", "A").as_deref(), Some("plain"));
        assert_eq!(release_value("AB=x\n", "A"), None);
    }

    #[test]
    fn sizes_are_binary_from_kib_to_tib_with_one_decimal() {
        assert_eq!(size(0), "0 KiB");
        assert_eq!(size(512), "0.5 KiB");
        assert_eq!(size(1023 * 1024), "1023.0 KiB");
        assert_eq!(size(1024 * 1024), "1.0 MiB");
        assert_eq!(size(1536 * 1024 * 1024), "1.5 GiB");
        assert_eq!(size(3 << 40), "3.0 TiB");
        // No unit past TiB.
        assert_eq!(size(2048 << 40), "2048.0 TiB");
        // Rounded to the nearest tenth.
        assert_eq!(size(1024 * 1024 + 52_429), "1.1 MiB");
        assert_eq!(size(1024 * 1024 + 52_428), "1.0 MiB");
        assert_eq!(size(u64::MAX), "16777216.0 TiB");
    }

    #[test]
    fn uptime_units_are_singular_or_plural_as_their_number_needs() {
        assert_eq!(uptime(0), "0 days, 0 hours, 0 mins");
        assert_eq!(uptime(86_400 + 3_600 + 60), "1 day, 1 hour, 1 min");
        assert_eq!(
            uptime(117 * 86_400 + 20 * 3_600 + 20 * 60 + 59),
            "117 days, 20 hours, 20 mins"
        );
        assert_eq!(uptime(86_399), "0 days, 23 hours, 59 mins");
    }

    #[test]
    fn load_has_two_decimals() {
        assert_eq!(load(0.5, 1.256, 12.0), "0.50, 1.26, 12.00");
    }

    #[test]
    fn virtualization_upper_cases_any_answer_but_none() {
        assert_eq!(virtualization("kvm\n"), "KVM");
        assert_eq!(virtualization("none\n"), "Dedicated");
        assert_eq!(virtualization("  \n"), "Dedicated");
        assert_eq!(virtualization(""), "Dedicated");
    }

    fn facts() -> Facts {
        Facts {
            cpu: Cpu {
                model: "Example CPU".to_owned(),
                cores: 8,
                mhz: Some("2400.000".to_owned()),
                cache: Some("32768 KB".to_owned()),
                aes: true,
                virt: false,
            },
            disk: Some(Usage {
                total: 100 << 30,
                used: 25 << 30,
            }),
            ram: Usage {
                total: 16 << 30,
                used: 4 << 30,
            },
            swap: Usage {
                total: 2 << 30,
                used: 0,
            },
            uptime: Some(90_061),
            load: Some((0.1, 0.2, 0.3)),
            os: Some("Ubuntu 24.04.1 LTS".to_owned()),
            arch: "x86_64".to_owned(),
            kernel: Some("6.8.0-1-generic".to_owned()),
            congestion: Some("bbr".to_owned()),
            virtualization: "KVM".to_owned(),
            online: Online {
                v4: true,
                v6: false,
            },
            isp: Isp {
                org: Some("AS1 Example".to_owned()),
                city: Some("Falkenstein".to_owned()),
                country: Some("DE".to_owned()),
                region: Some("Saxony".to_owned()),
            },
            ca_missing: false,
        }
    }

    fn rendered(facts: &Facts) -> String {
        table(facts).render(&Theme::plain(), false, None)
    }

    #[test]
    fn the_table_has_every_row_in_order() {
        let text = rendered(&facts());
        // One 21-wide label column, no rule between rows (ops spec,
        // "Layout").
        assert_eq!(
            text,
            "\
CPU Model            Example CPU
CPU Cores            8 @ 2400.000 MHz
CPU Cache            32768 KB
AES-NI               ✓ Enabled
VM-x/AMD-V           ✗ Disabled
Total Disk           100.0 GiB (25.0 GiB Used)
Total RAM            16.0 GiB (4.0 GiB Used)
Total Swap           2.0 GiB (0 KiB Used)
System Uptime        1 day, 1 hour, 1 min
Load Average         0.10, 0.20, 0.30
OS                   Ubuntu 24.04.1 LTS
Arch                 x86_64 (64 Bit)
Kernel               6.8.0-1-generic
TCP Congestion Ctrl  bbr
Virtualization       KVM
IPv4/IPv6            ✓ Online / ✗ Offline
Organization         AS1 Example
Location             Falkenstein / DE
Region               Saxony"
        );
    }

    /// Without the CA bundle the network rows are one Problem row with
    /// the fix (ops spec, decision 44), the other rows as they were.
    #[test]
    fn a_missing_ca_bundle_replaces_the_network_rows() {
        let text = rendered(&Facts {
            ca_missing: true,
            ..facts()
        });
        assert!(
            text.ends_with(&format!(
                "\nVirtualization       KVM\nIPv4/IPv6            Not checked: {}",
                sb_cli_core::tls::MISSING_CA
            )),
            "{text}"
        );
    }

    #[test]
    fn rows_without_a_value_are_left_out_or_blank() {
        let facts = Facts {
            cpu: Cpu {
                cores: 8,
                ..Cpu::default()
            },
            swap: Usage::default(),
            disk: None,
            uptime: None,
            load: None,
            os: None,
            kernel: None,
            congestion: None,
            ..facts()
        };
        let text = rendered(&facts);
        assert!(
            text.starts_with("CPU Model            CPU model not detected\n"),
            "{text}"
        );
        assert!(text.contains("\nCPU Cores            8\n"), "{text}");
        let no_cores = Facts {
            cpu: Cpu {
                mhz: Some("1.0".to_owned()),
                ..Cpu::default()
            },
            ..facts.clone()
        };
        // The table trims the cell's leading space.
        assert!(rendered(&no_cores).contains("\nCPU Cores            @ 1.0 MHz\n"));
        assert!(
            text.contains("\nAES-NI               ✗ Disabled\n"),
            "{text}"
        );
        assert!(
            text.contains("\nVM-x/AMD-V           ✗ Disabled\n"),
            "{text}"
        );
        for gone in ["CPU Cache", "Total Swap"] {
            assert!(!text.contains(gone), "{gone}\n{text}");
        }
        for row in [
            "Total Disk",
            "System Uptime",
            "Load Average",
            "OS",
            "Kernel",
            "TCP Congestion Ctrl",
        ] {
            let line = text
                .lines()
                .find(|line| line.starts_with(row))
                .unwrap_or_else(|| panic!("{row}\n{text}"));
            // Nothing after the label: the line ends there.
            assert_eq!(line, row, "{row}\n{text}");
        }
    }

    #[test]
    fn enabled_and_disabled_are_coloured_by_role() {
        use sb_cli_core::theme::{Palette, Profile};
        use sb_term_ui::style::paint;
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let text = table(&facts()).render(&theme, true, None);
        assert!(
            text.contains(&paint(&theme, Role::Ok, true, "✓ Enabled")),
            "{text:?}"
        );
        assert!(
            text.contains(&paint(&theme, Role::Problem, true, "✗ Disabled")),
            "{text:?}"
        );
    }

    /// A stand-in program printing `answer` and exiting `code`.
    fn stand_in(dir: &Path, name: &str, answer: &str, code: i32) -> PathBuf {
        let path = dir.join(name);
        sb_cli_core::testing::write_program(
            &path,
            format!("#!/bin/sh\nprintf '{answer}'\nexit {code}\n"),
        );
        path
    }

    /// Sources in `dir`: the given files, stand-ins, and two mounts.
    fn sources(dir: &Path) -> Sources {
        std::fs::write(dir.join("cpuinfo"), X86_64).expect("cpuinfo");
        std::fs::write(dir.join("meminfo"), MEMINFO).expect("meminfo");
        std::fs::write(
            dir.join("os-release"),
            "PRETTY_NAME=\"Ubuntu 22.04.4 LTS\"\nVERSION_ID=\"22.04\"\n",
        )
        .expect("os-release");
        std::fs::write(dir.join("cc"), "cubic\n").expect("cc");
        Sources {
            cpuinfo: dir.join("cpuinfo"),
            cpus: dir.join("no-cpus"),
            meminfo: dir.join("meminfo"),
            os_release: dir.join("os-release"),
            lsb_release: dir.join("no-lsb-release"),
            redhat_release: dir.join("no-redhat-release"),
            congestion: dir.join("cc"),
            detect_virt: stand_in(dir, "systemd-detect-virt", "kvm\\n", 0),
            zpool: stand_in(dir, "zpool", "1000\\t400\\n", 0),
            disks: || {
                Some(vec![Mounted {
                    dev: Some(1),
                    file_system: "ext4".to_owned(),
                    blocks: 10,
                    bfree: 4,
                    frsize: 1024,
                }])
            },
            disks_limit: DISKS_TIMEOUT,
            // An x86_64 machine whatever the host's, so the CPU rows are
            // the given cpuinfo's: on an ARM host sysinfo's name and
            // cpufreq's maximum would replace them.
            arch: || "x86_64".to_owned(),
            arm_brand: || unreachable!("asked for an ARM CPU's name on x86_64"),
        }
    }

    #[tokio::test]
    async fn gather_reads_the_given_sources_and_sums_total_disk() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let facts = gather(&sources(dir.path()), None, &CancellationToken::new())
            .await
            .expect("gather");
        assert_eq!(facts.cpu, cpu(X86_64, None));
        assert_eq!(facts.os.as_deref(), Some("Ubuntu 22.04.4 LTS"));
        // 22.04's procps 3.3 figures.
        assert_eq!(facts.ram.used, 530 * 1024);
        assert_eq!(facts.congestion.as_deref(), Some("cubic"));
        assert_eq!(facts.virtualization, "KVM");
        // df, the swap and the pool.
        assert_eq!(
            facts.disk,
            Some(Usage {
                total: 10 * 1024 + 200 * 1024 + 1000,
                used: 6 * 1024 + 50 * 1024 + 400,
            })
        );
        // This host's own values: present, whatever they are.
        assert!(facts.uptime.is_some_and(|up| up > 0));
        assert!(facts.load.is_some());
        assert_eq!(facts.arch, "x86_64");
        assert!(facts.kernel.is_some_and(|k| !k.is_empty()));
    }

    /// On ARM the CPU rows are lscpu's, as bench.sh's: sysinfo's name for
    /// the part and cpufreq's largest maximum, with cpuinfo's count and
    /// flags; whatever machine runs the test.
    #[tokio::test]
    async fn gather_on_arm_takes_the_parts_name_and_cpufreqs_maximum() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("aarch64"), AARCH64).expect("cpuinfo");
        for (cpu, khz) in [("cpu0", "2000000\n"), ("cpu1", "3000000\n")] {
            let freq = dir.path().join("cpus").join(cpu).join("cpufreq");
            std::fs::create_dir_all(&freq).expect("cpufreq");
            std::fs::write(freq.join("cpuinfo_max_freq"), khz).expect("max freq");
        }
        let sources = Sources {
            cpuinfo: dir.path().join("aarch64"),
            cpus: dir.path().join("cpus"),
            arch: || "aarch64".to_owned(),
            arm_brand: || "Neoverse-N1".to_owned(),
            ..sources(dir.path())
        };
        let facts = gather(&sources, None, &CancellationToken::new())
            .await
            .expect("gather");
        assert_eq!(
            facts.cpu,
            Cpu {
                model: "Neoverse-N1".to_owned(),
                cores: 2,
                mhz: Some("3000.0000".to_owned()),
                cache: None,
                aes: true,
                virt: false,
            }
        );
        assert_eq!(facts.arch, "aarch64");
    }

    #[test]
    fn the_default_machine_is_this_hosts() {
        assert_eq!((Sources::default().arch)(), std::env::consts::ARCH);
    }

    #[tokio::test]
    async fn gather_asks_the_network_rows_when_given_endpoints() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = crate::testing::NetworkFake::start("ADDRESS\n", r#"{"org":"AS1 Example"}"#)
            .expect("fake");
        let endpoints = fake.endpoints();
        let facts = gather(
            &sources(dir.path()),
            Some(&endpoints),
            &CancellationToken::new(),
        )
        .await
        .expect("gather");
        assert_eq!(facts.online, Online { v4: true, v6: true });
        assert_eq!(facts.isp.org.as_deref(), Some("AS1 Example"));
        let text = rendered(&facts);
        assert!(
            text.contains("\nIPv4/IPv6            ✓ Online / ✓ Online\n"),
            "{text}"
        );
        assert!(!text.contains("ADDRESS"), "{text}");
    }

    /// The host's facts come first, with no request; the network's rows
    /// are added after them (ops spec, decision 47: bench shows the
    /// local rows while the network is asked).
    #[tokio::test]
    async fn the_local_facts_come_before_the_network() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let fake = crate::testing::NetworkFake::start("ADDRESS\n", r#"{"org":"AS1 Example"}"#)
            .expect("fake");
        let endpoints = fake.endpoints();
        let token = CancellationToken::new();
        let local = gather_local(&sources(dir.path()), &token)
            .await
            .expect("local");
        assert_eq!(local.virtualization, "KVM");
        assert_eq!(local.online, Online::default());
        assert_eq!(local.isp, Isp::default());
        assert_eq!(fake.seen().len(), 0, "no request yet");
        let facts = with_network(local.clone(), Some(&endpoints), &token)
            .await
            .expect("network");
        assert_eq!(facts.online, Online { v4: true, v6: true });
        assert_eq!(facts.isp.org.as_deref(), Some("AS1 Example"));
        assert_eq!(
            Facts {
                online: local.online,
                isp: local.isp.clone(),
                ..facts.clone()
            },
            local
        );
        token.cancel();
        let err = with_network(local, None, &token)
            .await
            .expect_err("cancelled");
        assert_eq!(err.to_string(), "interrupted");
    }

    #[tokio::test]
    async fn missing_sources_are_blank_and_programs_add_nothing() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let given = sources(dir.path());
        let sources = Sources {
            cpuinfo: dir.path().join("none"),
            os_release: dir.path().join("none"),
            congestion: dir.path().join("none"),
            detect_virt: dir.path().join("none"),
            zpool: stand_in(dir.path(), "zpool-fails", "1\\t1\\n", 1),
            ..given.clone()
        };
        let facts = gather(&sources, None, &CancellationToken::new())
            .await
            .expect("gather");
        assert_eq!(facts.cpu, Cpu::default());
        assert_eq!(facts.os, None);
        // Not 22.04: procps 4's figures.
        assert_eq!(facts.ram.used, 400 * 1024);
        assert_eq!(facts.congestion, None);
        assert_eq!(facts.virtualization, "Dedicated");
        assert_eq!(
            facts.disk,
            Some(Usage {
                total: 10 * 1024 + 200 * 1024,
                used: 6 * 1024 + 50 * 1024,
            })
        );
        let sources = Sources {
            zpool: dir.path().join("no-zpool"),
            detect_virt: stand_in(dir.path(), "virt-none", "none\\n", 1),
            ..sources
        };
        let facts = gather(&sources, None, &CancellationToken::new())
            .await
            .expect("gather");
        assert_eq!(facts.virtualization, "Dedicated");
        assert_eq!(facts.disk.map(|disk| disk.total), Some(210 * 1024));
        let sources = Sources {
            meminfo: dir.path().join("none"),
            ..given
        };
        let err = gather(&sources, None, &CancellationToken::new())
            .await
            .expect_err("no meminfo");
        assert!(err.to_string().starts_with("reading "), "{err}");
    }

    #[tokio::test]
    async fn this_hosts_files_and_mounts_are_read() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let sources = Sources {
            detect_virt: dir.path().join("none"),
            zpool: dir.path().join("none"),
            ..Sources::default()
        };
        let facts = gather(&sources, None, &CancellationToken::new())
            .await
            .expect("gather");
        assert!(facts.cpu.cores > 0);
        assert!(facts.ram.total > 0 && facts.ram.used <= facts.ram.total);
        assert!(facts.swap.used <= facts.swap.total);
        assert!(facts.disk.is_some());
    }

    /// Mounts that take longer than any test's limit to list (a hung
    /// FUSE mount, briefly).
    const HUNG: fn() -> Option<Vec<Mounted>> = || {
        std::thread::sleep(Duration::from_millis(300));
        Some(vec![mounted(Some(1), "ext4", 1, 0)])
    };

    #[tokio::test]
    async fn mounts_not_listed_in_time_leave_total_disk_blank() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let sources = Sources {
            disks: HUNG,
            disks_limit: Duration::from_millis(20),
            ..sources(dir.path())
        };
        let facts = gather(&sources, None, &CancellationToken::new())
            .await
            .expect("gather");
        assert_eq!(facts.disk, None);
        // Mounts that can't be read leave it blank too, not short.
        let sources = Sources {
            disks: || None,
            ..sources
        };
        let facts = gather(&sources, None, &CancellationToken::new())
            .await
            .expect("gather");
        assert_eq!(facts.disk, None);
    }

    #[tokio::test]
    async fn a_cancelled_gather_is_interrupted_without_waiting_for_the_mounts() {
        let _spawns = crate::bench::SPAWNS.lock().await;
        let dir = tempfile::tempdir().expect("dir");
        let sources = Sources {
            disks: HUNG,
            disks_limit: Duration::from_secs(60),
            ..sources(dir.path())
        };
        let token = CancellationToken::new();
        token.cancel();
        let started = std::time::Instant::now();
        let err = gather(&sources, None, &token)
            .await
            .expect_err("interrupted");
        assert_eq!(err.to_string(), "interrupted");
        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn the_default_sources_are_the_hosts() {
        let sources = Sources::default();
        assert_eq!(sources.cpuinfo, Path::new("/proc/cpuinfo"));
        assert_eq!(sources.cpus, Path::new("/sys/devices/system/cpu"));
        assert_eq!(sources.meminfo, Path::new("/proc/meminfo"));
        assert_eq!(sources.os_release, Path::new("/etc/os-release"));
        assert_eq!(sources.lsb_release, Path::new("/etc/lsb-release"));
        assert_eq!(sources.redhat_release, Path::new("/etc/redhat-release"));
        assert_eq!(
            sources.congestion,
            Path::new("/proc/sys/net/ipv4/tcp_congestion_control")
        );
        assert_eq!(sources.detect_virt, Path::new("systemd-detect-virt"));
        assert_eq!(sources.zpool, Path::new("zpool"));
        assert_eq!(sources.disks_limit, Duration::from_secs(5));
        assert!((sources.disks)().is_some_and(|listed| !listed.is_empty()));
    }
}
