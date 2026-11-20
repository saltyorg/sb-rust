//! The whole-log search's matches (tui spec, "Whole-log search, save and
//! print"): every match the search knows, in log order, loaded or not,
//! as three parts: the older side's, those in the buffer when the search
//! started (and the follow's since), and the newer side's.
//!
//! - **Order:** the older part runs oldest to nearest, the newer part
//!   nearest to newest, so the parts joined are log order whichever order
//!   each side's matches come in (`Order`): a side that sends nearest
//!   first grows outward, one that sends farthest first grows inward.
//! - **The cap** (tui spec, "Matches kept"): at most `cap` a side. A side
//!   that sends nearest first stops itself there; for one that sends
//!   farthest first the farthest are dropped, so the nearest are kept.
//! - **Known:** a side that sends farthest first doesn't know its nearest
//!   match until it ends, so a jump into it waits for its end (`known`).
//! - **Positions** are the source's, compared by equality: a journal
//!   cursor has no order of its own outside sb-journal.

use std::collections::{HashSet, VecDeque};

use jiff::Timestamp;

use crate::source::{Found, Order, Position, SearchEnd, Side};

/// A match the search knows: its entry's position and how many matches
/// the entry holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) position: Position,
    pub(crate) count: usize,
}

/// How one side of the search stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SideState {
    /// The side reads (it was asked for and hasn't ended).
    pub(crate) running: bool,
    /// The order its matches come in, once one step has come.
    pub(crate) order: Option<Order>,
    pub(crate) end: Option<SearchEnd>,
}

impl SideState {
    fn asked(asked: bool) -> Self {
        Self {
            running: asked,
            order: None,
            end: None,
        }
    }

    /// Whether the part's match nearest the buffer is known: always for a
    /// side that sends nearest first, else once it has ended.
    fn nearest_known(&self) -> bool {
        !self.running || self.order == Some(Order::NearestFirst)
    }
}

/// The matches of one whole-log search.
#[derive(Debug)]
pub(crate) struct Whole {
    /// The viewer's count of its searches: a step of another is dropped.
    pub(crate) id: u64,
    /// When it started: the progress's ages are from here.
    pub(crate) started: Timestamp,
    older: VecDeque<Hit>,
    middle: Vec<Hit>,
    newer: VecDeque<Hit>,
    /// Every position held, so none is held twice.
    known: HashSet<Position>,
    /// The sum of the matches' counts.
    total: usize,
    cap: usize,
    pub(crate) older_side: SideState,
    pub(crate) newer_side: SideState,
    /// The time of the last entry a side read, for the progress.
    pub(crate) reached: Option<Timestamp>,
}

impl Whole {
    /// A search with `middle` (the buffer's matches, in log order) and
    /// the sides asked for.
    pub(crate) fn new(
        id: u64,
        started: Timestamp,
        middle: Vec<Hit>,
        older: bool,
        newer: bool,
        cap: usize,
    ) -> Self {
        let mut whole = Self {
            id,
            started,
            older: VecDeque::new(),
            middle: Vec::new(),
            newer: VecDeque::new(),
            known: HashSet::new(),
            total: 0,
            cap,
            older_side: SideState::asked(older),
            newer_side: SideState::asked(newer),
            reached: None,
        };
        for hit in middle {
            if whole.known.insert(hit.position.clone()) {
                whole.total += hit.count;
                whole.middle.push(hit);
            }
        }
        whole
    }

    /// A step of a side: its matches joined in log order, the cap kept,
    /// and its end.
    pub(crate) fn step(
        &mut self,
        side: Side,
        order: Order,
        found: Vec<Found>,
        reached: Option<Timestamp>,
        end: Option<SearchEnd>,
    ) {
        // The oldest reached, whichever side: the progress is how far
        // back the search has read (tui spec, "Progress").
        if let Some(reached) = reached {
            self.reached = Some(self.reached.map_or(reached, |old| old.min(reached)));
        }
        for f in found {
            if !self.known.insert(f.position.clone()) {
                continue;
            }
            let hit = Hit {
                position: f.position,
                count: f.count,
            };
            self.total += hit.count;
            let part = match side {
                Side::Older => &mut self.older,
                Side::Newer => &mut self.newer,
            };
            // Older nearest first and newer farthest first grow at the
            // older end of their part; the others at the newer end.
            let at_front = matches!(
                (side, order),
                (Side::Older, Order::NearestFirst) | (Side::Newer, Order::FarthestFirst)
            );
            if at_front {
                part.push_front(hit);
            } else {
                part.push_back(hit);
            }
            if part.len() > self.cap {
                // The farthest goes: the older part's oldest, the newer
                // part's newest.
                let gone = match side {
                    Side::Older => part.pop_front(),
                    Side::Newer => part.pop_back(),
                };
                if let Some(gone) = gone {
                    self.total -= gone.count;
                    self.known.remove(&gone.position);
                }
            }
        }
        let state = match side {
            Side::Older => &mut self.older_side,
            Side::Newer => &mut self.newer_side,
        };
        state.order = Some(order);
        if let Some(end) = end {
            state.running = false;
            state.end = Some(end);
        }
    }

    /// A match the follow brought, at the newest end.
    pub(crate) fn followed(&mut self, hit: Hit) {
        if !self.known.insert(hit.position.clone()) {
            return;
        }
        self.total += hit.count;
        if self.newer.is_empty() {
            self.middle.push(hit);
        } else {
            self.newer.push_back(hit);
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.older.len() + self.middle.len() + self.newer.len()
    }

    pub(crate) fn total(&self) -> usize {
        self.total
    }

    /// The `i`th match in log order.
    pub(crate) fn get(&self, i: usize) -> Option<&Hit> {
        let (o, m) = (self.older.len(), self.middle.len());
        if i < o {
            self.older.get(i)
        } else if i < o + m {
            self.middle.get(i - o)
        } else {
            self.newer.get(i - o - m)
        }
    }

    /// Where `position` is in log order.
    pub(crate) fn index_of(&self, position: &Position) -> Option<usize> {
        if !self.known.contains(position) {
            return None;
        }
        self.older
            .iter()
            .chain(&self.middle)
            .chain(&self.newer)
            .position(|hit| &hit.position == position)
    }

    /// The matches before the `i`th, counted as the header numbers them.
    pub(crate) fn count_before(&self, i: usize) -> usize {
        self.older
            .iter()
            .chain(&self.middle)
            .chain(&self.newer)
            .take(i)
            .map(|hit| hit.count)
            .sum()
    }

    /// Where the buffer was when the search started, with no match of its
    /// own: between the older part and the newer one.
    pub(crate) fn start_gap(&self) -> usize {
        self.older.len() + self.middle.len()
    }

    /// Whether the `i`th match can be jumped to, `None` past either end:
    /// in a part whose side still sends farthest first, its order isn't
    /// final yet.
    pub(crate) fn known(&self, i: usize) -> bool {
        let (o, m) = (self.older.len(), self.middle.len());
        if i < o {
            self.older_side.nearest_known()
        } else if i < o + m {
            true
        } else {
            i < self.len() && self.newer_side.nearest_known()
        }
    }

    /// Whether the newer side was asked for (reading or ended).
    pub(crate) fn read_newer(&self) -> bool {
        self.newer_side.running || self.newer_side.end.is_some()
    }

    /// Whether a side still reads.
    pub(crate) fn running(&self) -> bool {
        self.older_side.running || self.newer_side.running
    }

    /// Whether a side stopped at its cap.
    pub(crate) fn capped(&self) -> bool {
        [&self.older_side, &self.newer_side]
            .iter()
            .any(|s| s.end == Some(SearchEnd::Capped))
            || self.older.len() >= self.cap
            || self.newer.len() >= self.cap
    }

    /// Whether a side ended failed.
    pub(crate) fn failed(&self) -> bool {
        [&self.older_side, &self.newer_side]
            .iter()
            .any(|s| matches!(s.end, Some(SearchEnd::Failed(_))))
    }

    /// How far back the search has read: the age of the last entry a side
    /// read, from the search's start (tui spec, "Progress").
    pub(crate) fn back(&self) -> Option<String> {
        let reached = self.reached?;
        Some(age(self.started.duration_since(reached)))
    }
}

/// An age as the progress shows it: whole minutes under an hour, hours
/// under two days, then days; at least `1 min`. Why not jiff's friendly
/// printer (<https://docs.rs/jiff/0.2.38/jiff/fmt/friendly/struct.SpanPrinter.html>):
/// each of its designators is one label set (`min`/`hr`, or `m`/`h`/`d`),
/// none mixes `min`, `h` and `days`, and the spec's hours up to two days
/// need a rule of their own; indicatif's `HumanDuration` shows one unit,
/// rounded.
pub(crate) fn age(age: jiff::SignedDuration) -> String {
    let secs = age.as_secs().max(0);
    if secs < 3_600 {
        format!("{} min", (secs / 60).max(1))
    } else if secs < 2 * 86_400 {
        format!("{} h", secs / 3_600)
    } else {
        format!("{} days", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::position;

    fn hit(n: u64) -> Hit {
        Hit {
            position: position(n),
            count: 1,
        }
    }

    fn found(ns: &[u64]) -> Vec<Found> {
        ns.iter()
            .map(|&n| Found {
                position: position(n),
                time: None,
                count: 1,
            })
            .collect()
    }

    /// The matches' positions in log order.
    fn order(whole: &Whole) -> Vec<Position> {
        (0..whole.len())
            .map(|i| whole.get(i).expect("in range").position.clone())
            .collect()
    }

    fn positions(ns: &[u64]) -> Vec<Position> {
        ns.iter().map(|&n| position(n)).collect()
    }

    fn start(middle: &[u64], cap: usize) -> Whole {
        Whole::new(
            1,
            Timestamp::UNIX_EPOCH,
            middle.iter().map(|&n| hit(n)).collect(),
            true,
            true,
            cap,
        )
    }

    /// Each side's matches join in log order, whichever order they come in.
    #[test]
    fn the_parts_join_in_log_order() {
        for (older_order, newer_order) in [
            (Order::NearestFirst, Order::FarthestFirst),
            (Order::FarthestFirst, Order::NearestFirst),
        ] {
            let mut whole = start(&[50, 51], 10);
            let older: &[&[u64]] = match older_order {
                Order::NearestFirst => &[&[40, 30], &[20]],
                Order::FarthestFirst => &[&[20], &[30, 40]],
            };
            let newer: &[&[u64]] = match newer_order {
                Order::NearestFirst => &[&[60, 70], &[80]],
                Order::FarthestFirst => &[&[80], &[70, 60]],
            };
            for step in older {
                whole.step(Side::Older, older_order, found(step), None, None);
            }
            for step in newer {
                whole.step(Side::Newer, newer_order, found(step), None, None);
            }
            assert_eq!(order(&whole), positions(&[20, 30, 40, 50, 51, 60, 70, 80]));
            assert_eq!(whole.total(), 8);
            assert_eq!(whole.index_of(&position(60)), Some(5));
            assert_eq!(whole.count_before(5), 5);
        }
    }

    /// The cap keeps a side's nearest matches; a match is held once.
    #[test]
    fn the_cap_keeps_the_nearest() {
        let mut whole = start(&[], 3);
        whole.step(
            Side::Older,
            Order::FarthestFirst,
            found(&[1, 2, 3, 4, 5]),
            None,
            None,
        );
        assert_eq!(order(&whole), positions(&[3, 4, 5]));
        whole.step(
            Side::Newer,
            Order::FarthestFirst,
            found(&[90, 80, 70, 60]),
            None,
            None,
        );
        assert_eq!(order(&whole), positions(&[3, 4, 5, 60, 70, 80]));
        assert_eq!(whole.total(), 6);
        assert!(whole.capped());
        whole.step(Side::Older, Order::FarthestFirst, found(&[5]), None, None);
        assert_eq!(whole.total(), 6);
        assert_eq!(whole.index_of(&position(1)), None);
    }

    /// A side that sends farthest first isn't known until it ends; one
    /// that sends nearest first is known as it comes.
    #[test]
    fn a_farthest_first_side_is_known_at_its_end() {
        let mut whole = start(&[50], 10);
        whole.step(
            Side::Older,
            Order::FarthestFirst,
            found(&[10, 20]),
            None,
            None,
        );
        whole.step(Side::Newer, Order::NearestFirst, found(&[60]), None, None);
        assert!(!whole.known(1));
        assert!(whole.known(2));
        assert!(whole.known(3));
        assert!(!whole.known(4));
        whole.step(
            Side::Older,
            Order::FarthestFirst,
            Vec::new(),
            None,
            Some(SearchEnd::Done),
        );
        assert!(whole.known(1));
        assert!(whole.running());
        whole.step(
            Side::Newer,
            Order::NearestFirst,
            Vec::new(),
            None,
            Some(SearchEnd::Done),
        );
        assert!(!whole.running());
    }

    /// The follow's matches go at the newest end, once.
    #[test]
    fn followed_matches_go_at_the_end() {
        let mut whole = start(&[50], 10);
        whole.followed(hit(60));
        whole.followed(hit(60));
        assert_eq!(order(&whole), positions(&[50, 60]));
        whole.step(Side::Newer, Order::NearestFirst, found(&[70]), None, None);
        whole.followed(hit(80));
        assert_eq!(order(&whole), positions(&[50, 60, 70, 80]));
    }

    /// Where the buffer was at the start, between the parts; a part is
    /// known by its side's order, the middle always; a side that ended at
    /// its cap is capped however few it kept.
    #[test]
    fn the_parts_edges_and_the_cap() {
        let mut whole = start(&[50], 10);
        whole.step(
            Side::Older,
            Order::NearestFirst,
            found(&[40, 30]),
            None,
            None,
        );
        assert_eq!(whole.start_gap(), 3);
        whole.step(
            Side::Newer,
            Order::FarthestFirst,
            found(&[70, 60]),
            None,
            None,
        );
        assert!(whole.known(2), "the middle");
        assert!(
            !whole.known(3),
            "the newer part, still sending farthest first"
        );
        assert!(!whole.capped());
        whole.step(
            Side::Older,
            Order::NearestFirst,
            Vec::new(),
            None,
            Some(SearchEnd::Capped),
        );
        assert!(whole.capped());
        // Either part at the cap is capped, the other below it.
        let mut whole = start(&[], 2);
        whole.step(
            Side::Newer,
            Order::FarthestFirst,
            found(&[90, 80, 70]),
            None,
            None,
        );
        assert!(whole.capped());
        let mut whole = start(&[], 2);
        whole.step(
            Side::Older,
            Order::FarthestFirst,
            found(&[1, 2, 3]),
            None,
            None,
        );
        assert!(whole.capped());
    }

    /// The progress is how far back the search has read: the oldest time
    /// either side reached, kept when a later step (the newer side's)
    /// reached a newer one.
    #[test]
    fn the_progress_is_the_oldest_time_reached() {
        let started: Timestamp = "2026-11-20T12:00:00Z".parse().expect("time");
        let mut whole = Whole::new(1, started, Vec::new(), true, true, 10);
        assert_eq!(whole.back(), None);
        let ago = |hours: i64| started - jiff::SignedDuration::from_hours(hours);
        whole.step(
            Side::Older,
            Order::NearestFirst,
            found(&[5]),
            Some(ago(3)),
            None,
        );
        assert_eq!(whole.back().as_deref(), Some("3 h"));
        whole.step(
            Side::Newer,
            Order::FarthestFirst,
            found(&[9]),
            Some(ago(1)),
            None,
        );
        assert_eq!(whole.back().as_deref(), Some("3 h"));
        whole.step(
            Side::Older,
            Order::NearestFirst,
            found(&[2]),
            Some(ago(5)),
            None,
        );
        assert_eq!(whole.back().as_deref(), Some("5 h"));
        // A step that read nothing new leaves it.
        whole.step(Side::Newer, Order::FarthestFirst, Vec::new(), None, None);
        assert_eq!(whole.back().as_deref(), Some("5 h"));
    }

    #[test]
    fn ages_read_as_minutes_hours_and_days() {
        assert_eq!(age(jiff::SignedDuration::from_secs(3_600)), "1 h");
        let secs = jiff::SignedDuration::from_secs;
        assert_eq!(age(secs(5)), "1 min");
        assert_eq!(age(secs(40 * 60)), "40 min");
        assert_eq!(age(secs(5 * 3_600)), "5 h");
        assert_eq!(age(secs(47 * 3_600)), "47 h");
        assert_eq!(age(secs(2 * 86_400)), "2 days");
        assert_eq!(age(secs(-5)), "1 min");
    }
}
