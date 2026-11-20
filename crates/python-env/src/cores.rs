//! How many of the environment check's probes run at once: one per
//! physical core sb may run on, and no more than the CPUs it may use.
//!
//! A probe is a Python process, busy on a CPU for its whole run, so two
//! of them on SMT siblings of one core only share that core: a 1-core,
//! 2-thread CPU (an Atom N450 or D410) runs them in turn, as before.
//! `std::thread::available_parallelism` counts logical CPUs (the affinity
//! mask and a cgroup's CPU quota,
//! <https://doc.rust-lang.org/std/thread/fn.available_parallelism.html>);
//! the cores come from sysfs's CPU topology, each allowed CPU's
//! `core_cpus_list` (`thread_siblings_list` on older kernels): the CPUs
//! of its core, the same text for every CPU of one core
//! (<https://docs.kernel.org/admin-guide/cputopology.html>). The
//! affinity mask is rustix's `sched_getaffinity`.
//!
//! Why not a crate: `num_cpus::get_physical` (1.16) and sysinfo's
//! `System::physical_core_count` (0.40) count the machine's cores from
//! `/proc/cpuinfo`, whatever sb may run on, so a container or a cpuset of
//! two siblings would count every core. procfs's `CpuInfo` (0.18) indexes
//! `/proc/cpuinfo`'s entries, not CPU numbers, and arm64's has no core
//! ids; hwlocality binds the C library hwloc.

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::path::Path;

/// Where sysfs lists the CPUs.
const SYSFS_CPUS: &str = "/sys/devices/system/cpu";

/// The probes at once on this host: [`cap`] of the logical CPUs and the
/// physical cores of the CPUs sb may run on.
pub(crate) fn probes_at_once() -> NonZeroUsize {
    let logical = std::thread::available_parallelism().ok();
    // Fails (EINVAL) on a kernel built for more than rustix's 1024 CPUs:
    // the probes then run in turn.
    let allowed = rustix::thread::sched_getaffinity(None)
        .ok()
        .map(|set| cpus_in(&set));
    let physical = allowed.and_then(|cpus| physical_cores(Path::new(SYSFS_CPUS), &cpus));
    cap(logical, physical)
}

/// The CPUs `set` holds, by number.
fn cpus_in(set: &rustix::thread::CpuSet) -> Vec<usize> {
    (0..rustix::thread::CpuSet::MAX_CPU)
        .filter(|&cpu| set.is_set(cpu))
        .collect()
}

/// The physical cores `cpus` lie on, from the topology under `root` (a
/// `/sys/devices/system/cpu`): how many different sibling lists they
/// have. `None` when a CPU's can't be read, or there are no CPUs.
fn physical_cores(root: &Path, cpus: &[usize]) -> Option<NonZeroUsize> {
    let mut cores = HashSet::new();
    for cpu in cpus {
        let topology = root.join(format!("cpu{cpu}/topology"));
        let siblings = std::fs::read_to_string(topology.join("core_cpus_list"))
            .or_else(|_| std::fs::read_to_string(topology.join("thread_siblings_list")))
            .ok()?;
        let siblings = siblings.trim();
        if siblings.is_empty() {
            return None;
        }
        cores.insert(siblings.to_string());
    }
    NonZeroUsize::new(cores.len())
}

/// The probes at once: the physical cores, at most the logical CPUs
/// (a CPU quota below them, say); in turn when the cores can't be read.
fn cap(logical: Option<NonZeroUsize>, physical: Option<NonZeroUsize>) -> NonZeroUsize {
    match (logical, physical) {
        (_, None) => NonZeroUsize::MIN,
        // The logical CPUs unknown: the cores alone.
        (None, Some(physical)) => physical,
        (Some(logical), Some(physical)) => logical.min(physical),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("non-zero")
    }

    /// A fake `/sys/devices/system/cpu` whose CPU `i` has `lists[i]` as
    /// its `file` (`None`: no topology).
    fn sysfs(file: &str, lists: &[Option<&str>]) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("root");
        for (cpu, list) in lists.iter().enumerate() {
            let topology = root.path().join(format!("cpu{cpu}/topology"));
            std::fs::create_dir_all(&topology).expect("topology");
            if let Some(list) = list {
                std::fs::write(topology.join(file), format!("{list}\n")).expect("list");
            }
        }
        root
    }

    /// Two SMT siblings are one core; separate cores count each; only the
    /// CPUs sb may run on count.
    #[test]
    fn siblings_share_a_core() {
        // An Atom N450: one core, two threads.
        let atom = sysfs("core_cpus_list", &[Some("0-1"), Some("0-1")]);
        assert_eq!(physical_cores(atom.path(), &[0, 1]), Some(n(1)));
        // Two cores without SMT.
        let two = sysfs("core_cpus_list", &[Some("0"), Some("1")]);
        assert_eq!(physical_cores(two.path(), &[0, 1]), Some(n(2)));
        // Four threads on two cores, siblings listed as the kernel does
        // when they aren't neighbours (0 with 2, 1 with 3).
        let smt = sysfs(
            "core_cpus_list",
            &[Some("0,2"), Some("1,3"), Some("0,2"), Some("1,3")],
        );
        assert_eq!(physical_cores(smt.path(), &[0, 1, 2, 3]), Some(n(2)));
        assert_eq!(physical_cores(smt.path(), &[0, 2]), Some(n(1)));
        assert_eq!(physical_cores(smt.path(), &[1, 2]), Some(n(2)));
    }

    /// An older kernel's list is `thread_siblings_list`.
    #[test]
    fn an_older_kernels_list_is_read() {
        let old = sysfs(
            "thread_siblings_list",
            &[Some("0-1"), Some("0-1"), Some("2-3"), Some("2-3")],
        );
        assert_eq!(physical_cores(old.path(), &[0, 1, 2, 3]), Some(n(2)));
    }

    /// A CPU without a readable list, an empty one, or no CPUs at all:
    /// the cores are unknown.
    #[test]
    fn unknown_cores_are_none() {
        let missing = sysfs("core_cpus_list", &[Some("0"), None]);
        assert_eq!(physical_cores(missing.path(), &[0, 1]), None);
        assert_eq!(physical_cores(missing.path(), &[0, 7]), None);
        let empty = sysfs("core_cpus_list", &[Some("")]);
        assert_eq!(physical_cores(empty.path(), &[0]), None);
        assert_eq!(physical_cores(missing.path(), &[]), None);
    }

    /// The physical cores, no more than the logical CPUs; in turn when
    /// the cores are unknown.
    #[test]
    fn the_cap_is_the_cores_within_the_cpus() {
        assert_eq!(cap(Some(n(2)), Some(n(1))), n(1));
        assert_eq!(cap(Some(n(4)), Some(n(4))), n(4));
        assert_eq!(cap(Some(n(1)), Some(n(4))), n(1));
        assert_eq!(cap(None, Some(n(3))), n(3));
        assert_eq!(cap(Some(n(8)), None), n(1));
        assert_eq!(cap(None, None), n(1));
    }

    /// The affinity mask's CPUs, by number, sparse ones included.
    #[test]
    fn the_masks_cpus_are_listed() {
        let mut set = rustix::thread::CpuSet::new();
        for cpu in [0, 3, 40] {
            set.set(cpu);
        }
        assert_eq!(cpus_in(&set), [0, 3, 40]);
        assert_eq!(cpus_in(&rustix::thread::CpuSet::new()), Vec::<usize>::new());
    }

    /// A smoke test on this host's own mask and topology: it reads them
    /// without failing, within the CPUs sb may use.
    #[test]
    fn this_hosts_cap_is_read() {
        let mask = rustix::thread::sched_getaffinity(None).expect("mask");
        let cores = physical_cores(Path::new(SYSFS_CPUS), &cpus_in(&mask)).expect("cores");
        let logical = std::thread::available_parallelism().expect("cpus");
        assert_eq!(probes_at_once(), logical.min(cores));
    }
}
