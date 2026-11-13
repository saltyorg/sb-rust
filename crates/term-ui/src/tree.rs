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
use crate::text::wrap_line;

/// The column times end in at least (proto-runner's reviewed layout);
/// further right when a line needs it, never past the screen.
const COLUMN: usize = 64;
/// The least space between a label and its time, so an ending's reason
/// (`interrupted · 12s`) never reads as part of the label (the
/// maintainer, 2026-11-04, trying decision 11's first version); fewer
/// on a screen too narrow for three. Plain end lines use it too, always
/// in full (`runner::end_line`).
pub(crate) const GAP: usize = 3;
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
/// (<https://docs.rs/termtree/1.0.0/termtree/struct.GlyphPalette.html>).
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
    /// Why a failed step failed, short, before its time (a failed
    /// child's `exit 3`).
    reason: Option<String>,
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

/// A failed step's time column: its reason first, if it has one (`exit 3
/// · 2s`), for the live tree and plain end lines alike.
pub(crate) fn failed_time(reason: Option<&str>, secs: u64) -> String {
    match reason {
        Some(reason) => format!("{reason} · {secs}s"),
        None => format!("{secs}s"),
    }
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
            reason: None,
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

    /// A failed step's short reason, shown before its time.
    pub fn set_reason(&mut self, id: usize, reason: Option<String>) {
        self.nodes[id].reason = reason;
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
                failed_time(node.reason.as_deref(), secs),
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
    /// for it is cut with `…` to leave `GAP` spaces before the time.
    fn line(&self, id: usize, d: &Draw, column: usize) -> String {
        let Parts {
            lead,
            role,
            left,
            right,
            right_role,
        } = self.parts(id, d);
        // A screen too narrow for the whole gap gets fewer spaces.
        let spare = column.saturating_sub(width(&lead) + width(&right));
        let gap = GAP.min(spare).max(1);
        let room = spare.saturating_sub(gap);
        let left = cut(&left, room);
        let painted = d.paint(role, &left);
        let left = console::pad_str(&painted, room, console::Alignment::Left, None);
        let line = format!(
            "{}{left}{}{}",
            d.paint(Role::Faint, &lead),
            " ".repeat(gap),
            d.paint(right_role, &right)
        );
        // A screen too narrow even for the branch and the time: the whole
        // line is cut to it, so a live line never wraps (console keeps
        // escape codes whole; see `cut`).
        if width(&line) > column {
            console::truncate_str(&line, column, "…").into_owned()
        } else {
            line
        }
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
                width(&p.lead) + width(&p.left) + GAP + width(&p.right)
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
        // Notices, the failure block and kept output lines: cut to the
        // screen while live (the live area draws one row per line), and
        // in the finished tree whole, wrapped inside the tree: each
        // continuation row starts with the guide lines and indent of the
        // line it continues, so nothing is lost and no row reaches the
        // terminal's own wrap (ui-L4, plan decision 35). Below two
        // columns of room (a wide character needs two) it is cut.
        // termtree's `with_multiline` does this only for the items of a
        // whole tree it draws itself (see `GLYPHS`).
        let fit = |text: &str, room: usize| {
            if live || room < 2 {
                vec![cut(text, room)]
            } else {
                wrap_line(text, room, true)
            }
        };
        let mut lines: Vec<String> = Vec::new();
        if hidden > 0 {
            let count = format!("… {hidden} earlier");
            lines.push(format!("{lead}{}", d.paint(Role::Muted, &count)));
        }
        for notice in node.notices.iter().skip(hidden) {
            // A notice's continuation rows start under its text, past the
            // mark.
            for (i, text) in fit(&notice.text, room.saturating_sub(2)).iter().enumerate() {
                let row = match (i, notice.warn) {
                    (0, true) => format!("{} {text}", d.paint(Role::Warn, "!")),
                    (_, true) => format!("  {text}"),
                    (0, false) => d.paint(Role::Muted, &format!("› {text}")),
                    (_, false) => d.paint(Role::Muted, &format!("  {text}")),
                };
                lines.push(format!("{lead}{row}"));
            }
        }
        // The failure block: the error, every line, under the step.
        if node.status == Status::Failed {
            for line in &node.error {
                lines.extend(
                    fit(line, room)
                        .iter()
                        .map(|text| format!("{lead}{}", d.paint(Role::Problem, text))),
                );
            }
        }
        let shown = match node.status {
            Status::Running => self.tail_shown(d.rows),
            Status::Failed => KEPT,
            Status::Interrupted => TAIL,
            _ => 0,
        };
        let skip = node.tail.len().saturating_sub(shown);
        for line in node.tail.iter().skip(skip) {
            lines.extend(
                fit(line, room)
                    .iter()
                    .map(|text| format!("{lead}{}", d.paint(Role::Muted, text))),
            );
        }
        // Then where the full output is, when the run is logged; its
        // continuation rows start under its text, past the branch.
        if let (Status::Failed, Some(pointer)) = (node.status, &self.pointer) {
            let branch = format!("{}{}", GLYPHS.last_item, GLYPHS.item_indent);
            let under = " ".repeat(width(&branch));
            let texts = fit(&format!("full output: {pointer}"), room.saturating_sub(4));
            for (i, text) in texts.iter().enumerate() {
                let branch = if i == 0 { &branch } else { &under };
                lines.push(format!(
                    "{lead}{}{}",
                    d.paint(Role::Faint, branch),
                    d.paint(Role::Muted, text)
                ));
            }
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
    /// hangs under it. A tree taller than the screen collapses its oldest
    /// settled steps (each with its subtree and what hangs under it) to
    /// "… N finished" until it fits, so the running steps and their
    /// output stay in view (ui-M1, hardening plan decision 17); the
    /// finished tree still prints every step (`take_printable`).
    ///
    /// Why not indicatif or ratatui: both clip a draw taller than the
    /// terminal and neither picks what to keep. indicatif stops drawing
    /// the bar's lines at the terminal's height (`draw_to_term`, "Stop
    /// here if printing this bar would exceed the terminal height",
    /// <https://docs.rs/indicatif/0.18.6/src/indicatif/draw_target.rs.html>),
    /// which hid the running step at the bottom; ratatui's inline viewport
    /// (not a term-ui dependency) "is clamped to the current terminal
    /// height"
    /// (<https://docs.rs/ratatui/0.30.2/ratatui/enum.Viewport.html#variant.Inline>).
    /// What to collapse is the tree's own rule.
    pub fn live(&self, d: &Draw) -> Vec<String> {
        let column = self.column(d);
        let steps: Vec<(usize, Vec<String>)> = self.order[self.printed..]
            .iter()
            .map(|&id| {
                let mut lines = vec![self.line(id, d, column)];
                lines.extend(self.under(id, d, true));
                (id, lines)
            })
            .collect();
        let mut height: usize = steps.iter().map(|(_, lines)| lines.len()).sum();
        // Collapse settled steps from the top until the tree fits, each
        // with its whole subtree (settled too, and right after it in tree
        // order). A step joins the run of collapsed steps just above it
        // when it is a sibling of the run's first step (the step after a
        // collapsed subtree is its sibling or outside its parent); else
        // it starts a run of its own, one "… N finished" line.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut collapsed = vec![false; steps.len()];
        let mut i = 0;
        while i < steps.len() && height > d.rows {
            let id = steps[i].0;
            if !self.settled(id) {
                i += 1;
                continue;
            }
            let joins = i > 0
                && collapsed[i - 1]
                && runs
                    .last()
                    .is_some_and(|&(head, _)| self.nodes[head].parent == self.nodes[id].parent);
            if !joins {
                runs.push((id, 0));
                height += 1;
            }
            let end = i
                + 1
                + steps[i + 1..]
                    .iter()
                    .take_while(|(below, _)| self.descends(*below, id))
                    .count();
            for (j, (_, lines)) in steps.iter().enumerate().take(end).skip(i) {
                collapsed[j] = true;
                height -= lines.len();
            }
            if let Some((_, count)) = runs.last_mut() {
                *count += end - i;
            }
            i = end;
        }
        let mut runs = runs.into_iter();
        let mut out = Vec::new();
        for (i, (id, lines)) in steps.into_iter().enumerate() {
            if !collapsed[i] {
                out.extend(lines);
            } else if let Some(&(head, count)) = runs.as_slice().first()
                && head == id
            {
                runs.next();
                let lead = format!(
                    "{}{}{}",
                    self.indent(head),
                    GLYPHS.middle_item,
                    GLYPHS.item_indent
                );
                out.push(format!(
                    "{}{}",
                    d.paint(Role::Faint, &lead),
                    d.paint(Role::Muted, &format!("… {count} finished"))
                ));
            }
        }
        out
    }
}

/// `text` cut to `room` columns with `…` (console's cut, which keeps
/// escape codes whole, then no space left before the `…`;
/// <https://docs.rs/console/0.16.6/console/fn.truncate_str.html>), or whole
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
        ╰── ✗ Validating Saltbox configuration                interrupted · 12s
            ╰── ✗ Validating accounts.yml                     interrupted · 12s
                ├── ✗ Validating Cloudflare API credentials   interrupted · 12s
                ╰── ✓ Docker Hub credentials validated                       0s
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
            assert_eq!(tree.take_printable(d), [] as [String; 0]);
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
        assert_eq!(tree.live(&d), [] as [String; 0]);
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
        assert_eq!(tree.take_printable(&d), [] as [String; 0]);
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

    /// A failed child's reason comes before the step's time (`exit 3 ·
    /// 3s`), in the time column like "interrupted".
    #[test]
    fn a_failed_childs_reason_comes_before_its_time() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("Checking", "checked"), t0);
        tree.set_error(
            id,
            vec!["systemctl is-active [ARG] exited with code 3".into()],
        );
        tree.set_reason(id, Some("exit 3".into()));
        tree.finish(id, Status::Failed, secs(t0, 3));
        tree.end();
        let printed = tree.take_printable(&draw(&theme, secs(t0, 3))).join("\n");
        insta::assert_snapshot!(printed, @r"
        ╰── ✗ Checking                                       exit 3 · 3s
            systemctl is-active [ARG] exited with code 3
        ");
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

    /// In the finished tree a notice, an error line, a kept output line
    /// and the pointer longer than the screen wrap inside the tree, at
    /// words: each continuation row carries the guide lines and indent of
    /// the line it continues (a notice's under its text, past the mark),
    /// and no row is wider than the screen (ui-L4, plan decision 35).
    #[test]
    fn the_finished_tree_wraps_long_lines_inside_the_tree() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        tree.set_pointer(Some(
            "/var/log/sb/a-long-folder-name/for-the-run-logs/sb.log (run 1a2b)".into(),
        ));
        let all = tree.add(None, Labels::new("Setting up", "set up"), t0);
        let settings = tree.add(Some(all), Labels::new("Checking", "checked"), t0);
        tree.notice(
            settings,
            false,
            "downloads.path is /mnt/unionfs/downloads, which is on the merged mount rather than a local disk",
        );
        tree.notice(
            settings,
            true,
            "unknown key: downloads.extra_options_for_the_torrent_client (ignored; see the settings reference)",
        );
        tree.finish(settings, Status::Done, t0);
        let pull = tree.add(
            Some(all),
            Labels::new("Pulling", "pulled").failure("pull failed"),
            t0,
        );
        tree.output(pull, "Pulling ghcr.io/hotio/plex:release");
        tree.output(
            pull,
            "Error response from daemon: Head \"https://ghcr.io/v2/hotio/plex/manifests/release\": denied: denied",
        );
        tree.set_error(
            pull,
            vec!["docker pull ghcr.io/hotio/plex:release exited with code 1 after three attempts to reach the registry".into()],
        );
        tree.finish(pull, Status::Failed, t0);
        tree.finish(all, Status::SubStepFailed, t0);
        tree.end();
        let mut d = draw(&theme, t0);
        d.width = 80;
        let printed = tree.take_printable(&d);
        assert!(printed.iter().all(|l| width(l) <= 80), "{printed:#?}");
        insta::assert_snapshot!(printed.join("\n"), @r#"
        ╰── ✗ Setting up                            sub-step failed · 0s
            ├── ✓ checked                                             0s
            │   › downloads.path is /mnt/unionfs/downloads, which is on the merged mount
            │     rather than a local disk
            │   ! unknown key: downloads.extra_options_for_the_torrent_client (ignored;
            │     see the settings reference)
            ╰── ✗ pull failed                                         0s
                docker pull ghcr.io/hotio/plex:release exited with code 1 after three
                attempts to reach the registry
                Pulling ghcr.io/hotio/plex:release
                Error response from daemon: Head
                "https://ghcr.io/v2/hotio/plex/manifests/release": denied: denied
                ╰── full output: /var/log/sb/a-long-folder-name/for-the-run-logs/sb.log
                    (run 1a2b)
        "#);
    }

    /// A kept line's own indent and a blank kept line stay in the finished
    /// tree; a child's colour across a break is closed and reopened on
    /// each row; a narrow screen wraps within it; live, the same line is
    /// still cut (ui-L4).
    #[test]
    fn wrapped_kept_lines_keep_indent_blank_lines_and_colour() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(
            None,
            Labels::new("Running", "ran").failure("run failed"),
            t0,
        );
        tree.output(id, "    ^ here");
        tree.output(id, "");
        tree.output(
            id,
            "\x1b[31mred words that run on past the narrow screen\x1b[0m",
        );
        tree.set_error(id, vec!["failed".into()]);
        let mut d = draw(&theme, t0);
        d.colour = true;
        d.width = 36;
        let live = tree.live(&d);
        assert!(
            live.iter()
                .any(|l| console::strip_ansi_codes(l).ends_with('…')),
            "{live:#?}"
        );
        tree.finish(id, Status::Failed, t0);
        tree.end();
        let printed = tree.take_printable(&d);
        assert!(printed.iter().all(|l| width(l) <= 36), "{printed:#?}");
        let plain: Vec<String> = printed
            .iter()
            .map(|l| console::strip_ansi_codes(l).into_owned())
            .collect();
        assert!(plain.contains(&"        ^ here".to_owned()), "{plain:#?}");
        assert!(plain.contains(&"    ".to_owned()), "{plain:#?}");
        let red: Vec<&String> = printed.iter().filter(|l| l.contains("\x1b[31m")).collect();
        assert!(red.len() >= 2, "{printed:#?}");
        assert!(
            red.iter().all(|l| l.matches("\x1b[31m").count() == 1),
            "{red:#?}"
        );
    }

    /// Two columns of room wrap a finished kept line; less cuts it, and
    /// no room leaves its row empty (ui-L4).
    #[test]
    fn a_finished_line_wraps_from_two_columns_of_room() {
        let theme = plain();
        let t0 = Instant::now();
        let kept = |screen: usize| {
            let mut tree = Tree::default();
            let id = tree.add(None, Labels::new("Pulling", "pulled"), t0);
            tree.output(id, "abcd");
            tree.finish(id, Status::Interrupted, t0);
            tree.end();
            let mut d = draw(&theme, t0);
            d.width = screen;
            tree.take_printable(&d)[1..].to_vec()
        };
        // The lead is four columns wide.
        assert_eq!(kept(6), ["    ab", "    cd"]);
        assert_eq!(kept(5), ["    …"]);
        assert_eq!(kept(4), ["    "]);
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
        // 64 columns: the branch (4), the label, the gap (3) and "1s" (2).
        let label = format!("✓ {}", "x".repeat(53));
        let id = tree.add(None, Labels::new("x", &label[4..]), t0);
        tree.finish(id, Status::Done, secs(t0, 1));
        tree.end();
        let printed = tree.take_printable(&draw(&theme, secs(t0, 1)));
        assert_eq!(printed[0], format!("╰── {label}   1s"));
        assert_eq!(width(&printed[0]), COLUMN);
    }

    /// A screen too narrow even for a step's branch and time cuts the
    /// whole line to the screen, at any depth: no line wraps.
    #[test]
    fn no_line_is_wider_than_the_screen() {
        let theme = plain();
        let t0 = Instant::now();
        let interrupted = || {
            let mut tree = Tree::default();
            let top = tree.add(None, Labels::new("Pulling images", "images pulled"), t0);
            let deep = tree.add(Some(top), Labels::new("Pulling plex", "plex pulled"), t0);
            tree.finish(deep, Status::Interrupted, secs(t0, 12));
            tree.finish(top, Status::Interrupted, secs(t0, 12));
            tree.end();
            tree
        };
        for screen in [10, 18, 21, 22, 23, 24] {
            let d = Draw {
                width: screen,
                ..draw(&theme, secs(t0, 12))
            };
            let printed = interrupted().take_printable(&d);
            assert_eq!(printed.len(), 2, "{screen}");
            for line in printed {
                assert!(width(&line) <= screen, "{screen}: {line:?}");
            }
        }
    }

    /// One column more than the room cuts the label, and the gap stays.
    #[test]
    fn a_label_one_column_too_long_is_cut_before_the_gap() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("x", "x".repeat(54)), t0);
        tree.finish(id, Status::Done, secs(t0, 1));
        tree.end();
        let d = Draw {
            width: COLUMN,
            ..draw(&theme, secs(t0, 1))
        };
        let printed = tree.take_printable(&d);
        assert_eq!(printed[0], format!("╰── ✓ {}…   1s", "x".repeat(52)));
        assert_eq!(width(&printed[0]), COLUMN);
    }

    /// A screen too narrow for the whole gap shrinks it, here to the one
    /// column left, so the line still fits.
    #[test]
    fn a_narrow_screen_shrinks_the_gap() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let id = tree.add(None, Labels::new("Pulling images", "images pulled"), t0);
        tree.finish(id, Status::Interrupted, secs(t0, 12));
        tree.end();
        // The branch (4) and "interrupted · 12s" (17) leave one column:
        // a one-space gap, no label.
        let d = Draw {
            width: 22,
            ..draw(&theme, secs(t0, 12))
        };
        let printed = tree.take_printable(&d);
        assert_eq!(printed[0], "╰──  interrupted · 12s");
        assert_eq!(width(&printed[0]), 22);
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
        assert_eq!(tree.take_printable(&d), [] as [String; 0]);
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
        assert_eq!(printed[0], "╰── ✓ a very long done label a ver…   1s");
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
                assert_ne!(codes, [] as [String; 0]);
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

    /// A running step with 30 finished sub-steps above its running one, as
    /// ui-M1 found it: the tree a docker or validate-config run can grow.
    fn tall(t0: Instant) -> (Tree, usize, usize) {
        let mut tree = Tree::default();
        let all = tree.add(None, Labels::new("Starting containers", "started"), t0);
        for i in 0..30 {
            let app = tree.add(
                Some(all),
                Labels::new(format!("Starting app{i:02}"), format!("app{i:02} started")),
                t0,
            );
            tree.finish(app, Status::Done, secs(t0, 1));
        }
        let plex = tree.add(Some(all), Labels::new("Starting plex", "plex started"), t0);
        for j in 0..3 {
            tree.output(plex, format!("plex: waiting {j}"));
        }
        (tree, all, plex)
    }

    /// A live tree taller than the screen collapses its oldest settled
    /// lines to "… N finished", so the running step and its output stay
    /// in view (ui-M1, hardening plan decision 17); the newest finished
    /// steps fill what room is left.
    #[test]
    fn a_tree_taller_than_the_screen_keeps_the_running_step() {
        let theme = plain();
        let t0 = Instant::now();
        let (tree, _, _) = tall(t0);
        let mut d = draw(&theme, secs(t0, 2));
        d.width = 80;
        d.rows = 12;
        insta::assert_snapshot!(tree.live(&d).join("\n"), @r"
        ├── ⠋ Starting containers                                     2s
        │   ├── … 24 finished
        │   ├── ✓ app24 started                                       1s
        │   ├── ✓ app25 started                                       1s
        │   ├── ✓ app26 started                                       1s
        │   ├── ✓ app27 started                                       1s
        │   ├── ✓ app28 started                                       1s
        │   ├── ✓ app29 started                                       1s
        │   ├── ⠋ Starting plex                                       2s
        │   │   plex: waiting 0
        │   │   plex: waiting 1
        │   │   plex: waiting 2
        ");
    }

    /// The live tree follows the screen's height frame by frame: a
    /// terminal made shorter while live collapses more, a taller one
    /// shows everything again.
    #[test]
    fn a_resized_screen_collapses_to_its_new_height() {
        let theme = plain();
        let t0 = Instant::now();
        let (tree, _, _) = tall(t0);
        let mut d = draw(&theme, secs(t0, 2));
        d.rows = 40;
        let whole = tree.live(&d);
        assert_eq!(whole.len(), 35, "{whole:#?}");
        assert!(!whole.iter().any(|l| l.contains("finished")), "{whole:#?}");
        for rows in [20, 12, 8, 5] {
            d.rows = rows;
            let live = tree.live(&d);
            assert!(live.len() <= rows, "{rows}: {live:#?}");
            assert!(live[0].contains("Starting containers"), "{live:#?}");
            assert!(
                live.iter().any(|l| l.contains("Starting plex")),
                "{live:#?}"
            );
            assert!(
                live.iter().any(|l| l.ends_with("plex: waiting 2")),
                "{live:#?}"
            );
            let shown = live.iter().filter(|l| l.contains(" started ")).count();
            let collapsed = live
                .iter()
                .find_map(|l| l.split("… ").nth(1)?.strip_suffix(" finished"))
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            assert_eq!(shown + collapsed, 30, "{rows}: {live:#?}");
        }
        d.rows = 40;
        assert_eq!(tree.live(&d), whole);
    }

    /// Nothing collapsed is lost: the finished tree prints every step.
    #[test]
    fn a_collapsed_tree_prints_whole_when_done() {
        let theme = plain();
        let t0 = Instant::now();
        let (mut tree, all, plex) = tall(t0);
        let mut d = draw(&theme, secs(t0, 2));
        d.rows = 12;
        assert!(tree.live(&d).len() <= 12);
        tree.finish(plex, Status::Done, secs(t0, 2));
        tree.finish(all, Status::Done, secs(t0, 2));
        tree.end();
        let printed = tree.take_printable(&d);
        assert_eq!(printed.len(), 32, "{printed:#?}");
        assert!(
            !printed.iter().any(|l| l.contains("finished")),
            "{printed:#?}"
        );
        assert_eq!(tree.live(&d), Vec::<String>::new());
    }

    /// A settled subtree collapses whole, and the count is of steps; a
    /// failure block collapses with its step (the finished tree has it).
    #[test]
    fn settled_subtrees_and_failure_blocks_collapse_whole() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let all = tree.add(None, Labels::new("Validating", "validated"), t0);
        for i in 0..3 {
            let file = tree.add(
                Some(all),
                Labels::new(format!("File {i}"), format!("file {i}")),
                t0,
            );
            let check = tree.add(
                Some(file),
                Labels::new(format!("Check {i}"), format!("check {i}")),
                t0,
            );
            tree.finish(check, Status::Done, t0);
            tree.set_error(file, vec!["bad: one".into(), "bad: two".into()]);
            tree.finish(file, Status::Failed, t0);
        }
        let last = tree.add(Some(all), Labels::new("File 3", "file 3"), t0);
        let mut d = draw(&theme, secs(t0, 1));
        d.rows = 6;
        insta::assert_snapshot!(tree.live(&d).join("\n"), @r"
        ├── ⠋ Validating                                              1s
        │   ├── … 6 finished
        │   ├── ⠋ File 3                                              1s
        ");
        assert!(tree.is_running(last));
    }

    /// Settled steps at two depths collapse to a line each, at their own
    /// depth: a run doesn't join steps of another parent.
    #[test]
    fn nested_runs_collapse_at_their_own_depth() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let all = tree.add(None, Labels::new("Starting", "started"), t0);
        let done = |tree: &mut Tree, parent: usize, name: &str, n: usize| {
            for i in 0..n {
                let id = tree.add(
                    Some(parent),
                    Labels::new(format!("{name}{i}"), format!("{name}{i} done")),
                    t0,
                );
                tree.finish(id, Status::Done, t0);
            }
        };
        done(&mut tree, all, "a", 10);
        let inner = tree.add(Some(all), Labels::new("Inner", "inner done"), t0);
        done(&mut tree, inner, "b", 10);
        tree.add(Some(inner), Labels::new("Last", "last done"), t0);
        let mut d = draw(&theme, secs(t0, 1));
        d.rows = 8;
        insta::assert_snapshot!(tree.live(&d).join("\n"), @r"
        ├── ⠋ Starting                                                1s
        │   ├── … 10 finished
        │   ├── ⠋ Inner                                               1s
        │   │   ├── … 7 finished
        │   │   ├── ✓ b7 done                                         0s
        │   │   ├── ✓ b8 done                                         0s
        │   │   ├── ✓ b9 done                                         0s
        │   │   ├── ⠋ Last                                            1s
        ");
    }

    /// A screen too short even for the running steps collapses every
    /// settled step and leaves the rest to the terminal (indicatif draws
    /// from the top).
    #[test]
    fn a_screen_shorter_than_the_running_steps_collapses_everything() {
        let theme = plain();
        let t0 = Instant::now();
        let (tree, _, _) = tall(t0);
        let mut d = draw(&theme, secs(t0, 2));
        d.rows = 3;
        insta::assert_snapshot!(tree.live(&d).join("\n"), @r"
        ├── ⠋ Starting containers                                     2s
        │   ├── … 30 finished
        │   ├── ⠋ Starting plex                                       2s
        │   │   plex: waiting 2
        ");
    }

    /// Finished siblings on either side of a running one collapse to a
    /// line each: a run never reaches across a step still in view.
    #[test]
    fn a_running_step_splits_the_runs() {
        let theme = plain();
        let t0 = Instant::now();
        let mut tree = Tree::default();
        let all = tree.add(None, Labels::new("Starting", "started"), t0);
        let done = |tree: &mut Tree, name: &str| {
            for i in 0..5 {
                let id = tree.add(
                    Some(all),
                    Labels::new(format!("{name}{i}"), format!("{name}{i} done")),
                    t0,
                );
                tree.finish(id, Status::Done, t0);
            }
        };
        done(&mut tree, "a");
        tree.add(Some(all), Labels::new("B", "b done"), t0);
        done(&mut tree, "c");
        tree.add(Some(all), Labels::new("D", "d done"), t0);
        let mut d = draw(&theme, secs(t0, 1));
        d.rows = 6;
        insta::assert_snapshot!(tree.live(&d).join("\n"), @r"
        ├── ⠋ Starting                                                1s
        │   ├── … 5 finished
        │   ├── ⠋ B                                                   1s
        │   ├── … 4 finished
        │   ├── ✓ c4 done                                             0s
        │   ├── ⠋ D                                                   1s
        ");
    }
}
