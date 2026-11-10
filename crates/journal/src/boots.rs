//! The order between boots that systemd 255 and later put into
//! sd-journal's order between entries (src/libsystemd/sd-journal/
//! sd-journal.c, v255 and v259 alike): `compare_boot_ids`, called by
//! `compare_locations` (the merge's heads) and `compare_with_location`
//! (a head against the entry just taken, or the cursor) between the
//! monotonic and the realtime step, when the two boot ids differ.
//!
//! systemd's table, `newest_by_boot_id`, holds per boot id a priority
//! queue of the open files whose tail entry (`journal_file_read_tail_timestamp`,
//! read for every file sd-journal adds, matching or not) is of that boot,
//! the highest tail monotonic time first
//! (`journal_file_newest_monotonic_compare`). `compare_boot_ids` takes
//! each boot's first file (`journal_file_find_newest_for_boot_id`) and
//! orders the boots by those files' tail realtime, but only when both
//! files' header machine ids are equal; a boot not in the table, or two
//! machines, compare equal, and the realtime step decides, as in 249.
//! On equal tail monotonic times the file added first stays first: the
//! queue's `shuffle_up` (src/basic/prioq.c, v255 and v259) moves an item up
//! only past a parent that compares greater, and nothing is re-shuffled
//! for files that don't change (the tail is read again only when the
//! file's mtime, or in v259 its tail offset, changed). sb adds files in
//! its own file order, as for the merges' tie rule, where journalctl adds
//! a directory's files in readdir order. That matters only on a tie of
//! tail monotonic times within one boot, in practice two copies of one
//! entry (a flush), whose realtimes and machine are the same too.
//!
//! systemd 249 has no table (`journal_file_compare_locations`, v249
//! journal-file.c): `BootOrder::none`, which compares every pair of boots
//! equal.

use crate::file::Tail;
use itertools::Itertools;
use std::cmp::{Ordering, Reverse};
use std::collections::HashMap;

/// Each boot's newest file's tail, for ordering entries of different
/// boots. Built once per page from the files it opened.
#[derive(Debug)]
pub(crate) struct BootOrder {
    newest: HashMap<[u8; 16], Tail>,
    /// The release has the boot step (255 and later), even when the
    /// table is empty.
    active: bool,
}

impl BootOrder {
    /// No boot step: systemd 249's order.
    pub(crate) fn none() -> Self {
        Self {
            newest: HashMap::new(),
            active: false,
        }
    }

    /// Whether the release orders boots by the table (`of`), not
    /// `none`.
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    /// The table of the files' tails, in file order (`None` for a file
    /// with no tail: not in the table). Per boot, the tail with the
    /// highest monotonic time, the first on a tie.
    pub(crate) fn of(tails: impl IntoIterator<Item = Option<Tail>>) -> Self {
        // <https://docs.rs/itertools/0.14.0/itertools/structs/struct.GroupingMap.html#method.min_by_key>:
        // "If several elements are equally minimum, the first element is
        // picked", so the smallest `Reverse(monotonic)` keeps the first
        // file on a tie. Not `max_by_key`, which picks the last.
        let newest = tails
            .into_iter()
            .flatten()
            .into_grouping_map_by(|t| t.boot_id)
            .min_by_key(|_, t| Reverse(t.monotonic));
        Self {
            newest,
            active: true,
        }
    }

    /// `compare_boot_ids(a, b)`: by the boots' newest tails' realtime,
    /// when both are in the table and of one machine; else equal.
    pub(crate) fn compare(&self, a: &[u8; 16], b: &[u8; 16]) -> Ordering {
        match (self.newest.get(a), self.newest.get(b)) {
            (Some(x), Some(y)) if x.machine_id == y.machine_id => x.realtime.cmp(&y.realtime),
            _ => Ordering::Equal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(boot: u8, monotonic: u64, realtime: u64, machine: u8) -> Tail {
        Tail {
            boot_id: [boot; 16],
            monotonic,
            realtime,
            machine_id: [machine; 16],
        }
    }

    /// Boots of one machine order by their newest tail's realtime.
    #[test]
    fn boots_order_by_their_tails_realtime() {
        let t = BootOrder::of([Some(tail(1, 50, 300, 9)), Some(tail(2, 5, 400, 9))]);
        assert_eq!(t.compare(&[1; 16], &[2; 16]), Ordering::Less);
        assert_eq!(t.compare(&[2; 16], &[1; 16]), Ordering::Greater);
        assert_eq!(t.compare(&[1; 16], &[1; 16]), Ordering::Equal);
        let same = BootOrder::of([Some(tail(1, 50, 300, 9)), Some(tail(2, 5, 300, 9))]);
        assert_eq!(same.compare(&[1; 16], &[2; 16]), Ordering::Equal);
    }

    /// A boot's newest file is the one with the highest tail monotonic
    /// time, not realtime; on a tie, the first file.
    #[test]
    fn a_boots_newest_file_is_by_monotonic_time_then_first() {
        let t = BootOrder::of([
            Some(tail(1, 50, 300, 9)),
            Some(tail(2, 10, 400, 9)),
            Some(tail(2, 20, 200, 9)),
        ]);
        assert_eq!(t.compare(&[1; 16], &[2; 16]), Ordering::Greater);
        let t = BootOrder::of([
            Some(tail(1, 50, 300, 9)),
            Some(tail(2, 20, 200, 9)),
            Some(tail(2, 10, 400, 9)),
        ]);
        assert_eq!(t.compare(&[1; 16], &[2; 16]), Ordering::Greater);
        let tie = BootOrder::of([
            Some(tail(1, 50, 300, 9)),
            Some(tail(2, 20, 200, 9)),
            Some(tail(2, 20, 400, 9)),
        ]);
        assert_eq!(tie.compare(&[1; 16], &[2; 16]), Ordering::Greater);
        let tie = BootOrder::of([
            Some(tail(1, 50, 300, 9)),
            Some(tail(2, 20, 400, 9)),
            Some(tail(2, 20, 200, 9)),
        ]);
        assert_eq!(tie.compare(&[1; 16], &[2; 16]), Ordering::Less);
    }

    /// A boot not in the table, or two machines, compare equal (systemd
    /// returns 0 and the realtime step decides).
    #[test]
    fn a_missing_boot_or_another_machine_compares_equal() {
        let t = BootOrder::of([Some(tail(1, 50, 300, 9)), None, Some(tail(2, 5, 400, 8))]);
        assert_eq!(t.compare(&[1; 16], &[2; 16]), Ordering::Equal);
        assert_eq!(t.compare(&[1; 16], &[3; 16]), Ordering::Equal);
        assert_eq!(t.compare(&[3; 16], &[1; 16]), Ordering::Equal);
        assert_eq!(
            BootOrder::none().compare(&[1; 16], &[2; 16]),
            Ordering::Equal
        );
    }

    /// The machine id that counts is the boot's newest file's.
    #[test]
    fn the_machine_is_the_newest_files() {
        let t = BootOrder::of([
            Some(tail(1, 50, 300, 9)),
            Some(tail(2, 5, 400, 8)),
            Some(tail(2, 6, 450, 9)),
        ]);
        assert_eq!(t.compare(&[1; 16], &[2; 16]), Ordering::Less);
    }
}
