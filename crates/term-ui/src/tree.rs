//! The runner's tree (term-ui spec, "Task runner"; proto-runner's reviewed
//! look): steps in tree order (each parent's children in start order, each
//! subtree whole), each a branch with its label and time, the
//! times ending in one column, notices and output lines hanging under it.
//! Pure: it turns state into lines, and the runner decides where they go
//! (printed for good, or the live area).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use sb_cli_core::theme::{Role, Theme};

use crate::runner::Labels;
use crate::style::paint;

/// The column times end in at least (proto-runner's reviewed layout);
/// further right when a line needs it, never past the screen.
const COLUMN: usize = 64;
/// Output lines shown under a running or interrupted step: sb-go's 8
/// (`liveTaskOutputLines`).
pub(crate) const TAIL: usize = 8;
/// Output lines a failed step shows: its last 10 (the spec's failure block),
/// so each step keeps that many.
const KEPT: usize = 10;
/// The braille spinner (decision 7).
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// termtree's glyphs with a rounded last branch, the reviewed tree.
/// termtree draws only a finished tree in one go, so the runner borrows
/// its glyph set and draws branches line by line
/// (https://docs.rs/termtree/1.0.0/termtree/struct.GlyphPalette.html).
const GLYPHS: termtree::GlyphPalette = termtree::GlyphPalette {
    last_item: "╰",
    ..termtree::GlyphPalette::new()
};

/// How a step ended, or that it hasn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Running,
    Done,
    Failed,
    Interrupted,
    Stopped,
    SubStepFailed,
}

struct Notice {
    warn: bool,
    text: String,
}

struct Node {
    parent: Option<usize>,
    labels: Labels,
    status: Status,
    started: Instant,
    elapsed: Option<Duration>,
    tail: VecDeque<String>,
    notices: Vec<Notice>,
    /// A failed step's error, line by line, as it may be shown.
    error: Vec<String>,
}

/// What drawing needs besides the tree: the theme, whether colour is on,
/// the screen's size, the spinner frame and the time now.
pub(crate) struct Draw<'a> {
    pub theme: &'a Theme,
    pub colour: bool,
    pub width: usize,
    pub rows: usize,
    pub frame: usize,
    pub now: Instant,
}

impl Draw<'_> {
    fn paint(&self, role: Role, text: &str) -> String {
        paint(self.theme, role, self.colour, text)
    }
}

/// The steps under the run's title (the runner prints the title itself).
#[derive(Default)]
pub(crate) struct Tree {
    /// Steps by id, in start order.
    nodes: Vec<Node>,
    /// Step ids in tree order, the order they are drawn in: a step goes
    /// after its parent's last descendant, so a step started late inside
    /// a nested sibling still sits in its subtree. Kept here because
    /// termtree only draws a finished tree (see `GLYPHS`).
    order: Vec<usize>,
    /// Steps before this position in `order` are printed for good. Only a
    /// running step gains children and a running step isn't printed, so
    /// a new step always lands after them.
    printed: usize,
    ended: bool,
    /// Where the run's log is (`RunLog::pointer`), for a failure's block.
    pointer: Option<String>,
    /// The furthest right the times have ended: later lines, printed or
    /// live, never end left of it.
    column: usize,
}

/// A step line's pieces before its time column is known.
struct Parts {
    lead: String,
    role: Role,
    left: String,
    right: String,
    right_role: Role,
}

impl Tree {
    /// Start a step under `parent` (none: at the top).
    pub fn add(&mut self, parent: Option<usize>, labels: Labels, now: Instant) -> usize {
        let id = self.nodes.len();
        let at = match parent {
            Some(p) => self
                .order
                .iter()
                .rposition(|&n| n == p || self.descends(n, p))
                .map_or(self.order.len(), |last| last + 1),
            None => self.order.len(),
        };
        self.order.insert(at, id);
        self.nodes.push(Node {
            parent,
            labels,
            status: Status::Running,
            started: now,
            elapsed: None,
            tail: VecDeque::new(),
            notices: Vec::new(),
            error: Vec::new(),
        });
        id
    }

    /// Whether step `id` is under `ancestor`, at any depth.
    fn descends(&self, id: usize, ancestor: usize) -> bool {
        let mut at = self.nodes[id].parent;
        while let Some(p) = at {
            if p == ancestor {
                return true;
            }
            at = self.nodes[p].parent;
        }
        false
    }

    pub fn notice(&mut self, id: usize, warn: bool, text: impl Into<String>) {
        self.nodes[id].notices.push(Notice {
            warn,
            text: text.into(),
        });
    }

    /// One output line; only the newest [`KEPT`] are kept.
    pub fn output(&mut self, id: usize, line: impl Into<String>) {
        let tail = &mut self.nodes[id].tail;
        tail.push_back(line.into());
        if tail.len() > KEPT {
            tail.pop_front();
        }
    }

    /// Where the run's log is, shown under a failed step.
    pub fn set_pointer(&mut self, pointer: Option<String>) {
        self.pointer = pointer;
    }

    /// Where the run's log is, if it is logged.
    pub fn pointer(&self) -> Option<&str> {
        self.pointer.as_deref()
    }

    /// A failed step's error, line by line, shown under it.
    pub fn set_error(&mut self, id: usize, lines: Vec<String>) {
        self.nodes[id].error = lines;
    }

    /// Every step's status, in start order (for tests).
    #[cfg(test)]
    pub fn statuses(&self) -> Vec<Status> {
        self.nodes.iter().map(|n| n.status).collect()
    }

    /// Whether step `id` is still running.
    pub fn is_running(&self, id: usize) -> bool {
        self.nodes[id].status == Status::Running
    }

    /// End step `id`, once: a later ending is ignored.
    pub fn finish(&mut self, id: usize, status: Status, now: Instant) {
        let node = &mut self.nodes[id];
        if node.status == Status::Running {
            node.status = status;
            node.elapsed = Some(now.duration_since(node.started));
        }
    }

    /// The run is over: the last top-level step's place is known.
    pub fn end(&mut self) {
        self.ended = true;
    }

    /// Whether `id` is its parent's last child, once known: a later sibling
    /// says no; a finished parent (or the run's end, at the top) says yes.
    fn last(&self, id: usize) -> Option<bool> {
        let parent = self.nodes[id].parent;
        if self.nodes[id + 1..].iter().any(|n| n.parent == parent) {
            return Some(false);
        }
        let parent_done = match parent {
            Some(p) => self.nodes[p].status != Status::Running,
            None => self.ended,
        };
        parent_done.then_some(true)
    }

    /// The columns in front of `id`'s branch: "│   " under an ancestor
    /// that isn't last, spaces under one that is.
    fn indent(&self, id: usize) -> String {
        let mut columns = Vec::new();
        let mut at = self.nodes[id].parent;
        while let Some(p) = at {
            columns.push(self.skip(p));
            at = self.nodes[p].parent;
        }
        columns.reverse();
        columns.concat()
    }

    /// What continues `id`'s branch below its own line.
    fn skip(&self, id: usize) -> String {
        let skip = match self.last(id) {
            Some(true) => GLYPHS.last_skip,
            _ => GLYPHS.middle_skip,
        };
        format!("{skip}{}", GLYPHS.skip_indent)
    }

    /// Step `id`'s line before its time column is known: the branch, the
    /// label for its status, and its time (with the ending's reason).
    fn parts(&self, id: usize, d: &Draw) -> Parts {
        let node = &self.nodes[id];
        let item = match self.last(id) {
            Some(true) => GLYPHS.last_item,
            _ => GLYPHS.middle_item,
        };
        let lead = format!("{}{item}{}", self.indent(id), GLYPHS.item_indent);
        let secs = node
            .elapsed
            .unwrap_or_else(|| d.now.duration_since(node.started))
            .as_secs();
        let running = &node.labels.running;
        let (role, left, right) = match node.status {
            Status::Running => (
                Role::Accent,
                format!("{} {running}", SPINNER[d.frame % SPINNER.len()]),
                format!("{secs}s"),
            ),
            Status::Done => (
                Role::Ok,
                format!("✓ {}", node.labels.done),
                format!("{secs}s"),
            ),
            Status::Failed => (
                Role::Problem,
                format!("✗ {}", node.labels.failure.as_deref().unwrap_or(running)),
                format!("{secs}s"),
            ),
            Status::Interrupted => (
                Role::Warn,
                format!("✗ {running}"),
                format!("interrupted · {secs}s"),
            ),
            Status::Stopped => (
                Role::Warn,
                format!("✗ {running}"),
                format!("stopped · {secs}s"),
            ),
            Status::SubStepFailed => (
                Role::Warn,
                format!("✗ {running}"),
                format!("sub-step failed · {secs}s"),
            ),
        };
        let right_role = match node.status {
            Status::Running | Status::Done => Role::Muted,
            _ => role,
        };
        Parts {
            lead,
            role,
            left,
            right,
            right_role,
        }
    }

    /// Step `id`'s line, its time ending in `column`; a label too long
    /// for it is cut with `…`.
    fn line(&self, id: usize, d: &Draw, column: usize) -> String {
        let Parts {
            lead,
            role,
            left,
            right,
            right_role,
        } = self.parts(id, d);
        let room = column.saturating_sub(width(&lead) + width(&right) + 1);
        let left = cut(&left, room);
        let painted = d.paint(role, &left);
        let left = console::pad_str(&painted, room, console::Alignment::Left, None);
        format!(
            "{}{left} {}",
            d.paint(Role::Faint, &lead),
            d.paint(right_role, &right)
        )
    }

    /// Where the times of the steps not yet printed end: one column, past
    /// every one's whole line, so no label is cut while the screen has
    /// room (decision 11); at least `COLUMN`, never left of where times
    /// have ended before (so the live area doesn't jump left when a label
    /// shrinks), never past the screen. Why not tabled or indicatif:
    /// tabled lays out a whole table at once and indicatif's template
    /// widths are fixed, so neither keeps one column that only moves
    /// right across printed and live lines.
    fn column(&self, d: &Draw) -> usize {
        let needed = self.order[self.printed..]
            .iter()
            .map(|&id| {
                let p = self.parts(id, d);
                width(&p.lead) + width(&p.left) + 1 + width(&p.right)
            })
            .max()
            .unwrap_or(0);
        needed.max(self.column).max(COLUMN).min(d.width)
    }

    /// Lines under `id`: its notices (bullets: `›` info, orange `!`
    /// warning), then its newest output lines while it runs, or kept after
    /// a failure or an interruption.
    fn under(&self, id: usize, d: &Draw, live: bool) -> Vec<String> {
        let node = &self.nodes[id];
        let lead_text = format!("{}{}", self.indent(id), self.skip(id));
        let lead = d.paint(Role::Faint, &lead_text);
        // Live, a step shows its newest notices under a count of the
        // earlier ones, so many warnings can't outgrow the screen; the
        // finished tree prints them all.
        let cap = (d.rows / 4).clamp(1, TAIL);
        let hidden = if live {
            node.notices.len().saturating_sub(cap)
        } else {
            0
        };
        let room = d.width.saturating_sub(width(&lead_text));
        let mut lines: Vec<String> = Vec::new();
        if hidden > 0 {
            let count = format!("… {hidden} earlier");
            lines.push(format!("{lead}{}", d.paint(Role::Muted, &count)));
        }
        lines.extend(node.notices.iter().skip(hidden).map(|notice| {
            let text = cut(&notice.text, room.saturating_sub(2));
            let mark = if notice.warn {
                format!("{} {text}", d.paint(Role::Warn, "!"))
            } else {
                d.paint(Role::Muted, &format!("› {text}"))
            };
            format!("{lead}{mark}")
        }));
        // The failure block: the error, every line, under the step. Cut to
        // the screen only while live (a wrapped live line would break the
        // redraw); printed for good, nothing of it is lost.
        let fit = |text: &str, room: usize| {
            if live {
                cut(text, room)
            } else {
                text.to_string()
            }
        };
        if node.status == Status::Failed {
            lines.extend(
                node.error
                    .iter()
                    .map(|line| format!("{lead}{}", d.paint(Role::Problem, &fit(line, room)))),
            );
        }
        let shown = match node.status {
            Status::Running => self.tail_shown(d.rows),
            Status::Failed => KEPT,
            Status::Interrupted => TAIL,
            _ => 0,
        };
        let skip = node.tail.len().saturating_sub(shown);
        lines.extend(
            node.tail
                .iter()
                .skip(skip)
                .map(|line| format!("{lead}{}", d.paint(Role::Muted, &cut(line, room)))),
        );
        // Then where the full output is, when the run is logged.
        if let (Status::Failed, Some(pointer)) = (node.status, &self.pointer) {
            let branch = format!("{}{}", GLYPHS.last_item, GLYPHS.item_indent);
            let text = fit(&format!("full output: {pointer}"), room.saturating_sub(4));
            lines.push(format!(
                "{lead}{}{}",
                d.paint(Role::Faint, &branch),
                d.paint(Role::Muted, &text)
            ));
        }
        lines
    }

    /// Output lines shown per running step: up to 8, all running steps'
    /// tails together within half the screen's height (decision 7).
    fn tail_shown(&self, rows: usize) -> usize {
        let running = self
            .nodes
            .iter()
            .filter(|n| n.status == Status::Running && !n.tail.is_empty())
            .count()
            .max(1);
        (rows / 2 / running).clamp(1, TAIL)
    }

    /// Whether `id` and everything under it have ended.
    fn settled(&self, id: usize) -> bool {
        self.nodes[id].status != Status::Running
            && self
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| n.parent == Some(id))
                .all(|(child, _)| self.settled(child))
    }

    /// Take the lines that can be printed for good, in tree order: each
    /// step whose subtree has ended and whose place (middle or last) is
    /// known (decision 3).
    pub fn take_printable(&mut self, d: &Draw) -> Vec<String> {
        let mut out = Vec::new();
        let column = self.column(d);
        let mut printed = self.printed;
        // A bounded loop over the steps not yet printed.
        for &next in &self.order[self.printed..] {
            if !self.settled(next) || self.last(next).is_none() {
                break;
            }
            out.push(self.line(next, d, column));
            out.extend(self.under(next, d, false));
            printed += 1;
        }
        self.printed = printed;
        self.column = self.column.max(column);
        out
    }

    /// The live lines: every step not yet printed, in tree order, with what
    /// hangs under it.
    pub fn live(&self, d: &Draw) -> Vec<String> {
        let column = self.column(d);
        self.order[self.printed..]
            .iter()
            .flat_map(|&id| {
                std::iter::once(self.line(id, d, column)).chain(self.under(id, d, true))
            })
            .collect()
    }
}

/// `text` cut to `room` columns with `…` (console's cut, which keeps
/// escape codes whole, then no space left before the `…`;
/// https://docs.rs/console/0.16.6/console/fn.truncate_str.html), or whole
/// when it fits.
fn cut(text: &str, room: usize) -> String {
    if room == 0 {
        String::new()
    } else if width(text) > room {
        let cut = console::truncate_str(text, room.saturating_sub(1), "");
        format!("{}…", cut.trim_end())
    } else {
        text.to_string()
    }
}

/// Display width, escape codes left out.
fn width(text: &str) -> usize {
    console::measure_text_width(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_cli_core::theme::{Palette, Profile};

    fn plain() -> Theme {
        Theme::resolve(&Palette::default(), Profile::Ascii)
    }

    fn draw(theme: &Theme, now: Instant) -> Draw<'_> {
        Draw {
            theme,
            colour: false,
            width: 100,
            rows: 30,
            frame: 0,
            now,
        }
    }

    fn secs(start: Instant, s: u64) -> Instant {
        start + Duration::from_secs(s)
    }

    /// The tree validate-config draws when Ctrl-C stops its checks.
    fn interrupted_checks(t0: Instant) -> Tree {
        let mut tree = Tree::default();
        let all = tree.add(
            None,
            Labels::new(
                "Validating Saltbox configuration",
                "Saltbox configuration validated",
            ),
            t0,
        );
        let accounts = tree.add(
            Some(all),
            Labels::new("Validating accounts.yml", "Validated accounts.yml"),
            t0,
        );
        let cf = tree.add(
            Some(accounts),
            Labels::new(
                "Validating Cloudflare API credentials",
                "Cloudflare API credentials validated",
            ),
            t0,
        );
        let hub = tree.add(
            Some(accounts),
            Labels::new(
                "Validating Docker Hub credentials",
                "Docker Hub credentials validated",
            ),
            t0,
        );
        tree.finish(hub, Status::Done, t0);
        for id in [cf, accounts, all] {
            tree.finish(id, Status::Interrupted, secs(t0, 12));
        }
        tree.end();
        tree
    }

    /// No label is cut while the screen has room: the time column moves
    /// right as far as the tree's longest line needs (the maintainer's
    /// choice at config Checkpoint B), still one column.
    #[test]
    fn the_time_column_fits_the_tree() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = interrupted_checks(t0);
        let d = draw(&theme, secs(t0, 13));
        let printed = tree.take_printable(&d).join("\n");
        insta::assert_snapshot!(printed, @r"
        ╰── ✗ Validating Saltbox configuration              interrupted · 12s
            ╰── ✗ Validating accounts.yml                   interrupted · 12s
                ├── ✗ Validating Cloudflare API credentials interrupted · 12s
                ╰── ✓ Docker Hub credentials validated                     0s
        ");
        assert!(!printed.contains('…'), "{printed}");
        let ends: Vec<usize> = printed.lines().map(width).collect();
        assert!(ends.windows(2).all(|w| w[0] == w[1]), "{printed}");
    }

    /// Once a long line is printed, later lines keep its column, so the
    /// times stay in one column down the screen.
    #[test]
    fn the_time_column_never_moves_left() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let long = tree.add(
            None,
            Labels::new(
                "Validating a step whose label is far longer than the usual column",
                "done",
            ),
            t0,
        );
        tree.finish(long, Status::Interrupted, secs(t0, 1));
        let short = tree.add(None, Labels::new("Short", "short done"), t0);
        let d = draw(&theme, secs(t0, 2));
        let printed = tree.take_printable(&d);
        let live = tree.live(&d);
        assert_eq!(printed.len(), 1, "{printed:?}");
        assert_eq!(width(&printed[0]), width(&live[0]), "{printed:?} {live:?}");
        assert!(width(&live[0]) > COLUMN, "{live:?}");
        // Printed in turn, the short line keeps the column too.
        tree.finish(short, Status::Done, secs(t0, 2));
        tree.end();
        let printed = tree.take_printable(&d);
        assert_eq!(printed.len(), 1, "{printed:?}");
        assert_eq!(width(&printed[0]), width(&live[0]), "{printed:?}");
    }

    /// A live step's line pushes the column right as its time grows, and
    /// the column stays when its label shrinks (a short done label), so
    /// the live area never jumps left, even before anything is printed.
    #[test]
    fn the_live_column_grows_and_stays() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let label = "Downloading and verifying a large archive from the mirror";
        let step = tree.add(None, Labels::new(label, "archive ready"), t0);
        let at = |s: u64, frame: usize| Draw {
            frame,
            ..draw(&theme, secs(t0, s))
        };
        let width_at = |tree: &mut Tree, d: &Draw| {
            assert!(tree.take_printable(d).is_empty());
            width(&tree.live(d)[0])
        };
        let nine = width_at(&mut tree, &at(9, 0));
        assert!(nine > COLUMN);
        // Spinner frames are all one column wide.
        assert_eq!(width_at(&mut tree, &at(9, 3)), nine);
        let ten = width_at(&mut tree, &at(10, 0));
        assert_eq!(ten, nine + 1);
        tree.finish(step, Status::Done, secs(t0, 10));
        assert_eq!(width_at(&mut tree, &at(11, 0)), ten);
    }

    /// A screen narrower than the tree still cuts labels with `…`, and
    /// nothing is wider than the screen.
    #[test]
    fn a_narrow_screen_still_cuts_labels() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = interrupted_checks(t0);
        let d = Draw {
            width: 50,
            ..draw(&theme, secs(t0, 13))
        };
        let printed = tree.take_printable(&d);
        assert!(printed.iter().any(|l| l.contains('…')), "{printed:?}");
        assert!(printed.iter().all(|l| width(l) <= 50), "{printed:?}");
    }

    /// The spike's tour, ended: the reviewed tree, line for line.
    #[test]
    fn a_finished_run_draws_the_reviewed_tree() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let read = tree.add(
            None,
            Labels::new("Reading accounts.yml", "accounts.yml read"),
            t0,
        );
        tree.notice(read, false, "3 accounts");
        tree.finish(read, Status::Done, secs(t0, 1));
        let check = tree.add(
            None,
            Labels::new("Checking settings.yml", "settings.yml checked"),
            t0,
        );
        tree.notice(check, true, "unknown key: downloads.extra");
        tree.finish(check, Status::Done, secs(t0, 1));
        let creds = tree.add(
            None,
            Labels::new("Checking credentials", "credentials valid"),
            t0,
        );
        let cf = tree.add(
            Some(creds),
            Labels::new("Checking Cloudflare token", "Cloudflare token valid"),
            t0,
        );
        let hub = tree.add(
            Some(creds),
            Labels::new("Checking Docker Hub login", "Docker Hub login valid"),
            t0,
        );
        tree.finish(hub, Status::Done, secs(t0, 1));
        tree.finish(cf, Status::Done, secs(t0, 2));
        tree.finish(creds, Status::Done, secs(t0, 2));
        let report = tree.add(None, Labels::new("Writing report", "report written"), t0);
        tree.finish(report, Status::Done, t0);
        tree.end();
        let d = draw(&theme, secs(t0, 3));
        let printed = tree.take_printable(&d).join("\n");
        insta::assert_snapshot!(printed, @r"
        ├── ✓ accounts.yml read                                       1s
        │   › 3 accounts
        ├── ✓ settings.yml checked                                    1s
        │   ! unknown key: downloads.extra
        ├── ✓ credentials valid                                       2s
        │   ├── ✓ Cloudflare token valid                              2s
        │   ╰── ✓ Docker Hub login valid                              1s
        ╰── ✓ report written                                          0s
        ");
        assert!(tree.live(&d).is_empty());
    }

    /// While a step runs, finished ones above it are printed, the newest
    /// finished one stays live until its place is known, and the running
    /// one shows its spinner and output tail.
    #[test]
    fn the_newest_finished_step_stays_live_until_its_place_is_known() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let read = tree.add(
            None,
            Labels::new("Reading accounts.yml", "accounts.yml read"),
            t0,
        );
        tree.finish(read, Status::Done, secs(t0, 1));
        let d = draw(&theme, secs(t0, 1));
        assert!(tree.take_printable(&d).is_empty());
        assert_eq!(tree.live(&d).len(), 1, "the finished step waits");
        let check = tree.add(
            None,
            Labels::new("Checking settings.yml", "settings.yml checked"),
            t0,
        );
        tree.output(check, "checking item 1 of 11");
        tree.output(check, "checking item 2 of 11");
        let d = draw(&theme, secs(t0, 2));
        let printed = tree.take_printable(&d).join("\n");
        insta::assert_snapshot!(printed, @"├── ✓ accounts.yml read                                       1s");
        insta::assert_snapshot!(tree.live(&d).join("\n"), @r"
        ├── ⠋ Checking settings.yml                                   2s
        │   checking item 1 of 11
        │   checking item 2 of 11
        ");
    }

    /// Endings other than done: ✗ with the failure label, or the running
    /// label with why after it, in the time column.
    #[test]
    fn other_endings_say_why_in_the_time_column() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let failed = tree.add(
            None,
            Labels::new("Pulling", "pulled").failure("pull failed"),
            t0,
        );
        tree.output(failed, "error: timeout");
        tree.finish(failed, Status::Failed, secs(t0, 4));
        for status in [Status::Interrupted, Status::Stopped, Status::SubStepFailed] {
            let id = tree.add(None, Labels::new("Starting", "started"), t0);
            tree.finish(id, status, secs(t0, 1));
        }
        tree.end();
        let printed = tree.take_printable(&draw(&theme, secs(t0, 5))).join("\n");
        insta::assert_snapshot!(printed, @r"
        ├── ✗ pull failed                                             4s
        │   error: timeout
        ├── ✗ Starting                                  interrupted · 1s
        ├── ✗ Starting                                      stopped · 1s
        ╰── ✗ Starting                              sub-step failed · 1s
        ");
    }

    /// In the live area a step shows its newest 8 notices under a count of
    /// the earlier ones, so many warnings can't outgrow the screen; the
    /// finished tree keeps them all.
    #[test]
    fn live_notices_are_capped_and_printed_in_full() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("Checking", "checked"), t0);
        for n in 0..12 {
            tree.notice(id, true, format!("warning {n}"));
        }
        let mut d = draw(&theme, t0);
        d.rows = 40;
        let live = tree.live(&d);
        assert_eq!(live.len(), 1 + 1 + TAIL, "{live:#?}");
        assert_eq!(live[1], "│   … 4 earlier");
        assert_eq!(live[2], "│   ! warning 4");
        // A small screen shows fewer: a quarter of its rows.
        d.rows = 12;
        assert_eq!(tree.live(&d).len(), 1 + 1 + 3);
        tree.finish(id, Status::Done, t0);
        tree.end();
        let printed = tree.take_printable(&d);
        assert_eq!(printed.len(), 1 + 12);
        assert_eq!(printed[1], "    ! warning 0");
    }

    /// A notice wider than the screen is cut with `…` too.
    #[test]
    fn wide_notices_are_cut() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("Pulling", "pulled"), t0);
        tree.notice(id, true, "w".repeat(200));
        let mut d = draw(&theme, t0);
        d.width = 40;
        assert_eq!(tree.live(&d)[1], format!("│   ! {}…", "w".repeat(33)));
    }

    /// Text that exactly fills its room isn't cut; no room gives nothing.
    #[test]
    fn cut_keeps_what_fits() {
        assert_eq!(cut("abc", 3), "abc");
        assert_eq!(cut("abcd", 3), "ab…");
        assert_eq!(cut("abc", 0), "");
    }

    /// An output line wider than the screen is cut with `…`.
    #[test]
    fn wide_output_lines_are_cut() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("Pulling", "pulled"), t0);
        tree.output(id, "x".repeat(200));
        let mut d = draw(&theme, t0);
        d.width = 40;
        let live = tree.live(&d);
        assert_eq!(live[1], format!("│   {}…", "x".repeat(35)));
    }

    /// A failed step shows its error, every line, its last 10 output lines
    /// and where the full output is (the failure block); without a log, no
    /// pointer.
    #[test]
    fn a_failed_step_shows_the_failure_block() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        tree.set_pointer(Some("/var/log/sb/sb.log (run 1a2b)".into()));
        let id = tree.add(
            None,
            Labels::new("Pulling", "pulled").failure("pull failed"),
            t0,
        );
        for n in 0..12 {
            tree.output(id, format!("line {n}"));
        }
        tree.set_error(id, vec!["pull failed: timeout".into(), "after 30s".into()]);
        tree.finish(id, Status::Failed, secs(t0, 3));
        tree.end();
        let printed = tree.take_printable(&draw(&theme, secs(t0, 3))).join("\n");
        insta::assert_snapshot!(printed, @r"
        ╰── ✗ pull failed                                             3s
            pull failed: timeout
            after 30s
            line 2
            line 3
            line 4
            line 5
            line 6
            line 7
            line 8
            line 9
            line 10
            line 11
            ╰── full output: /var/log/sb/sb.log (run 1a2b)
        ");
        // Live, a long pointer is cut to the screen; printed, it is whole.
        let mut live = Tree::default();
        live.set_pointer(Some(
            "/a/very/long/path/to/the/log/sb.log (run 1a2b)".into(),
        ));
        let id = live.add(None, Labels::new("Pulling", "pulled"), t0);
        live.set_error(id, vec!["timeout".into()]);
        live.finish(id, Status::Failed, t0);
        let mut narrow = draw(&theme, t0);
        narrow.width = 30;
        let pointer = live.live(&narrow).pop().unwrap_or_default();
        assert!(
            pointer.ends_with('…') && width(&pointer) <= 30,
            "{pointer:?}"
        );
        live.end();
        let printed = live.take_printable(&narrow);
        assert!(
            printed.iter().any(|l| l.ends_with("(run 1a2b)")),
            "{printed:?}"
        );
        let mut again = Tree::default();
        let id = again.add(None, Labels::new("Pulling", "pulled"), t0);
        again.set_error(id, vec!["timeout".into()]);
        again.finish(id, Status::Failed, t0);
        again.end();
        let printed = again.take_printable(&draw(&theme, t0));
        assert!(
            !printed.iter().any(|l| l.contains("full output")),
            "{printed:?}"
        );
    }

    /// An interrupted step keeps its last lines in the finished tree.
    #[test]
    fn an_interrupted_step_keeps_its_last_lines() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("Pulling", "pulled"), t0);
        tree.output(id, "layer 3 of 7");
        tree.finish(id, Status::Interrupted, t0);
        tree.end();
        let printed = tree.take_printable(&draw(&theme, t0));
        assert_eq!(printed.last().map(String::as_str), Some("    layer 3 of 7"));
    }

    /// A step ends once: a later ending doesn't change it.
    #[test]
    fn a_step_ends_once() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("A", "a"), t0);
        tree.finish(id, Status::Done, secs(t0, 1));
        tree.finish(id, Status::Failed, secs(t0, 9));
        tree.end();
        let printed = tree.take_printable(&draw(&theme, secs(t0, 9)));
        assert_eq!(printed[0], format!("╰── ✓ a{}1s", " ".repeat(55)));
    }

    /// A label that exactly fills the room before the time isn't cut.
    #[test]
    fn a_label_that_just_fits_is_whole() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        // 64 columns: the branch (4), the label, a space and "1s" (2).
        let label = format!("✓ {}", "x".repeat(55));
        let id = tree.add(None, Labels::new("x", &label[4..]), t0);
        tree.finish(id, Status::Done, secs(t0, 1));
        tree.end();
        let printed = tree.take_printable(&draw(&theme, secs(t0, 1)));
        assert_eq!(printed[0], format!("╰── {label} 1s"));
    }

    /// A step that ended while a sub-step still runs stays live, even once
    /// a later sibling shows where it sits.
    #[test]
    fn a_step_waits_for_its_sub_steps() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let parent = tree.add(None, Labels::new("Parent", "parent done"), t0);
        let _child = tree.add(Some(parent), Labels::new("Child", "child done"), t0);
        tree.finish(parent, Status::Done, t0);
        let _next = tree.add(None, Labels::new("Next", "next done"), t0);
        let d = draw(&theme, t0);
        assert!(tree.take_printable(&d).is_empty());
        assert_eq!(tree.live(&d).len(), 3);
    }

    /// A label too long for the time column is cut with `…`, and a narrow
    /// screen moves the column in.
    #[test]
    fn long_labels_are_cut_so_times_stay_in_their_column() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(
            None,
            Labels::new("x", "a very long done label ".repeat(4)),
            t0,
        );
        tree.finish(id, Status::Done, secs(t0, 1));
        tree.end();
        let mut d = draw(&theme, secs(t0, 1));
        d.width = 40;
        let printed = tree.take_printable(&d);
        assert_eq!(printed[0], "╰── ✓ a very long done label a very…  1s");
        assert_eq!(width(&printed[0]), 40);
    }

    /// Two running steps with output share half the screen's height.
    #[test]
    fn running_tails_share_half_the_screen() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let parent = tree.add(None, Labels::new("Checking", "checked"), t0);
        let a = tree.add(Some(parent), Labels::new("A", "a"), t0);
        let b = tree.add(Some(parent), Labels::new("B", "b"), t0);
        for i in 0..TAIL + 2 {
            tree.output(a, format!("a{i}"));
            tree.output(b, format!("b{i}"));
        }
        let mut d = draw(&theme, t0);
        d.rows = 12;
        let live = tree.live(&d);
        let tails = live
            .iter()
            .filter(|l| l.ends_with(char::is_numeric))
            .count();
        assert_eq!(tails, 6, "{live:#?}");
        d.rows = 100;
        let live = tree.live(&d);
        assert!(live.iter().any(|l| l.ends_with("a9")) && !live.iter().any(|l| l.ends_with("a1")));
        assert_eq!(
            live.iter()
                .filter(|l| l.ends_with(char::is_numeric))
                .count(),
            2 * TAIL
        );
    }

    /// Every colour code the tree writes is of the profile's kind, and
    /// `ascii` writes none (success criterion 2).
    #[test]
    fn tree_colours_follow_the_profile() {
        use crate::style::sgr::{colour_codes, of_kind};
        let t0 = Instant::now();
        for profile in [
            Profile::TrueColor,
            Profile::Ansi256,
            Profile::Ansi,
            Profile::Ascii,
        ] {
            let theme = Theme::resolve(&Palette::default(), profile);
            let mut tree = Tree::default();
            let done = tree.add(None, Labels::new("A", "a"), t0);
            tree.notice(done, false, "info");
            tree.notice(done, true, "warning");
            tree.finish(done, Status::Done, t0);
            for status in [Status::Failed, Status::Interrupted] {
                let id = tree.add(None, Labels::new("B", "b"), t0);
                tree.output(id, "out");
                tree.finish(id, status, t0);
            }
            let running = tree.add(None, Labels::new("C", "c"), t0);
            tree.output(running, "out");
            let mut d = draw(&theme, t0);
            d.colour = true;
            let mut text = tree.take_printable(&d).join("\n");
            text.push_str(&tree.live(&d).join("\n"));
            let codes = colour_codes(&text);
            if profile == Profile::Ascii {
                assert!(!text.contains('\x1b'), "{text:?}");
            } else {
                assert!(!codes.is_empty());
                assert!(
                    codes.iter().all(|c| of_kind(c, profile)),
                    "{profile:?}: {codes:?}"
                );
            }
        }
    }

    /// With colour on, the glyphs are faint, the done mark green, the
    /// spinner accent and the time muted.
    #[test]
    fn colour_follows_the_roles() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("Pulling", "pulled"), t0);
        let mut d = draw(&theme, t0);
        d.colour = true;
        let live = tree.live(&d).join("");
        assert!(
            live.contains(&paint(&theme, Role::Accent, true, "⠋ Pulling")),
            "{live:?}"
        );
        assert!(
            live.contains(&paint(&theme, Role::Faint, true, "├── ")),
            "{live:?}"
        );
        assert!(
            live.contains(&paint(&theme, Role::Muted, true, "0s")),
            "{live:?}"
        );
        tree.finish(id, Status::Done, t0);
        tree.end();
        let printed = tree.take_printable(&d).join("");
        assert!(
            printed.contains(&paint(&theme, Role::Ok, true, "✓ pulled")),
            "{printed:?}"
        );
    }

    /// A step started inside a nested sibling after a later outer sibling
    /// is drawn in its subtree, live and printed: tree order, not start
    /// order.
    #[test]
    fn nested_steps_are_drawn_in_tree_order() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let check = tree.add(None, Labels::new("Checking", "checked"), t0);
        let inner = tree.add(Some(check), Labels::new("Inner", "inner done"), t0);
        let outer = tree.add(Some(check), Labels::new("Outer", "outer done"), t0);
        let leaf = tree.add(Some(inner), Labels::new("Leaf", "leaf done"), t0);
        let d = draw(&theme, secs(t0, 1));
        insta::assert_snapshot!(tree.live(&d).join("\n"), @r"
        ├── ⠋ Checking                                                1s
        │   ├── ⠋ Inner                                               1s
        │   │   ├── ⠋ Leaf                                            1s
        │   ├── ⠋ Outer                                               1s
        ");
        tree.finish(leaf, Status::Done, secs(t0, 1));
        tree.finish(inner, Status::Done, secs(t0, 1));
        tree.finish(outer, Status::Done, secs(t0, 1));
        tree.finish(check, Status::Done, secs(t0, 1));
        tree.end();
        insta::assert_snapshot!(tree.take_printable(&d).join("\n"), @r"
        ╰── ✓ checked                                                 1s
            ├── ✓ inner done                                          1s
            │   ╰── ✓ leaf done                                       1s
            ╰── ✓ outer done                                          1s
        ");
    }
}
