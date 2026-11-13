//! The log viewer (tui spec, "The viewer"): a buffer of a source's lines,
//! scrolled by screen rows with no selection cursor, paged at both ends,
//! following at the bottom.
//!
//! - **Windowed:** only the entries the window and the paging edges need
//!   are measured; a wrapped entry's height is ratatui's
//!   `Paragraph::line_count` (`unstable-rendered-line-info`, ADR-001;
//!   <https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Paragraph.html#method.line_count>),
//!   never the whole buffer's (proto-logs' fix).
//! - **Drawn** entry by entry as `Paragraph`s with `Wrap { trim: false }`,
//!   the first scrolled past its rows above the window
//!   (<https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Paragraph.html#method.scroll>).
//!   Why not ratatui's `List`: its items don't wrap. Why not one
//!   `Paragraph` of the whole buffer, or tui-scrollview: both lay out
//!   every entry each frame, a cost that grows with the buffer (20,000
//!   entries of up to 64 KiB), which proto-logs measured and replaced
//!   with this window.
//! - **Paging:** within 5 screens of the oldest loaded line, with more
//!   older, the page before is asked for; within 5 screens of the newest,
//!   with more newer and not following, the page after. One request at a
//!   time per direction.
//! - **The buffer** holds at most 20,000 entries and 32 MiB of message
//!   bytes; past either, the end far from the view is dropped, and the
//!   dropped side has more again. Dropping the newest end stops following.
//! - **Follow:** the source follows; scrolling up pauses the view (it
//!   stays put while lines come), and End, `G`, `f` or reaching the bottom
//!   resumes it. `f` while following at the bottom stops the follow.

use std::collections::VecDeque;
use std::sync::Arc;

use crossterm::event::{Event as Input, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line as TextLine, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use sb_cli_core::theme::Role;
use sb_term_ui::DisplayText;
use tokio::sync::{Notify, mpsc};

use crate::line::{self, Format};
use crate::source::{
    Event, Generation, Line, Notice, Outbox, PageKind, Position, Request, SourceHandle, Tagged,
};
use crate::term::{App, Flow};

/// The buffer's caps (tui decision 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Entries held.
    pub entries: usize,
    /// Message bytes held.
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 20_000,
            bytes: 32 << 20,
        }
    }
}

/// Screens of rows within which the next page is asked for (sb-go's
/// prefetch).
const PREFETCH_SCREENS: usize = 5;

/// What kind of source the viewer shows: its name's role in the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Service,
    Container,
}

impl Kind {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Kind::Service => "service",
            Kind::Container => "container",
        }
    }

    pub(crate) fn role(self) -> Role {
        match self {
            Kind::Service => Role::Service,
            Kind::Container => Role::Container,
        }
    }
}

/// How the viewer was left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leave {
    /// Esc: back to the picker.
    Back,
    /// `q`: quit, exit 0.
    Quit,
}

/// One entry of the buffer.
#[derive(Clone, Debug)]
enum Entry {
    Line(Line),
    /// A source notice, drawn as its own row (tui spec, "Viewer notices").
    Notice(Notice),
}

/// One end of the buffer: where the next page there starts, whether
/// there is more, and whether that page is asked for already (one request
/// at a time per direction).
#[derive(Clone, Debug, Default)]
struct Edge {
    /// The page edge to ask from (`Event::Page`'s `oldest` or `newest`, or
    /// the edge line's after a drop or a follow).
    position: Option<Position>,
    more: bool,
    asked: bool,
}

/// The follow: off, or on with the window at the bottom or paused above
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Follow {
    Off,
    Stuck,
    Paused,
}

/// Where the window starts: an item (`-1` the start-of-log marker, then
/// the buffer's entries, then the end marker) and the rows of it above
/// the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Anchor {
    item: isize,
    skip: usize,
}

/// The log viewer: an [`App`] for [`crate::Tui::run`].
#[derive(Debug)]
pub struct Viewer {
    name: String,
    kind: Kind,
    source: SourceHandle,
    events: mpsc::UnboundedReceiver<Tagged>,
    format: Format,
    limits: Limits,
    entries: VecDeque<Entry>,
    bytes: usize,
    /// The page edges to ask from (`Event::Page`'s `oldest` and `newest`,
    /// or the edge line's after a drop or a follow).
    older: Edge,
    newer: Edge,
    /// The first page has come.
    loaded: bool,
    /// The source follows.
    follow: Follow,
    wrap: bool,
    /// The window's top when not sticking.
    anchor: Anchor,
    /// The log area's size at the last draw.
    width: u16,
    height: usize,
    /// The last page's kind: a notice after an older page goes at the top.
    last_page: Option<PageKind>,
    leave: Option<Leave>,
}

impl Viewer {
    /// Open a source and show its newest lines, following (tui spec,
    /// "Opening"). `open` starts the source on the [`Outbox`] it is given;
    /// `wake` is the screen's ([`crate::Tui::waker`]).
    pub fn open(
        name: impl Into<String>,
        kind: Kind,
        format: Format,
        wake: Arc<Notify>,
        open: impl FnOnce(Outbox) -> SourceHandle,
    ) -> Self {
        Self::with_limits(name, kind, format, Limits::default(), wake, open)
    }

    /// [`Viewer::open`] with the buffer's caps given (the tests' small
    /// limits).
    pub fn with_limits(
        name: impl Into<String>,
        kind: Kind,
        format: Format,
        limits: Limits,
        wake: Arc<Notify>,
        open: impl FnOnce(Outbox) -> SourceHandle,
    ) -> Self {
        let (tx, events) = mpsc::unbounded_channel();
        let source = open(Outbox::new(tx, wake, Generation::default()));
        source.send(Request::Tail);
        source.send(Request::Follow(true));
        // Names are untrusted text too: through `DisplayText`, one line.
        let name: String = name.into();
        let name = DisplayText::from_bytes(name.as_bytes(), false)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        Self {
            name,
            kind,
            source,
            events,
            format,
            limits,
            entries: VecDeque::new(),
            bytes: 0,
            older: Edge::default(),
            newer: Edge::default(),
            loaded: false,
            follow: Follow::Stuck,
            wrap: true,
            anchor: Anchor { item: 0, skip: 0 },
            width: 0,
            height: 0,
            last_page: None,
            leave: None,
        }
    }

    /// How the viewer was left, once its run has ended with a quit.
    #[must_use]
    pub fn leave(&self) -> Option<Leave> {
        self.leave
    }

    // --- events from the source ---

    fn apply(&mut self, event: Event) {
        match event {
            Event::Page {
                kind,
                lines,
                oldest,
                newest,
                more_older,
                more_newer,
            } => self.page(kind, lines, oldest, newest, more_older, more_newer),
            Event::Followed(lines) => {
                // A notice after this is the follow's, at the bottom.
                self.last_page = None;
                if let Some(last) = lines.last() {
                    self.newer.position = Some(last.position.clone());
                }
                self.append(lines);
            }
            Event::Notice(notice) => {
                // The follow has stopped or never started (follow-L4): the
                // view stays where it is, paged, and `f` retries.
                let cant_follow = matches!(notice, Notice::CantFollow(_));
                if cant_follow && self.following() {
                    if self.stuck() {
                        self.anchor = self.bottom_anchor();
                    }
                    self.follow = Follow::Off;
                }
                let of_follow = notice.of_follow();
                let entry = Entry::Notice(notice);
                if self.last_page == Some(PageKind::Older) && !of_follow {
                    self.entries.push_front(entry);
                    self.anchor.item += 1;
                } else {
                    self.entries.push_back(entry);
                }
            }
        }
        self.enforce_limits();
    }

    fn page(
        &mut self,
        kind: PageKind,
        lines: Vec<Line>,
        oldest: Option<Position>,
        newest: Option<Position>,
        more_older: bool,
        more_newer: bool,
    ) {
        self.last_page = Some(kind);
        match kind {
            PageKind::Tail => {
                self.entries.clear();
                self.bytes = 0;
                self.older = Edge {
                    position: oldest,
                    more: more_older,
                    asked: false,
                };
                self.newer = Edge {
                    position: newest,
                    more: more_newer,
                    asked: false,
                };
                self.loaded = true;
                self.append(lines);
                if !self.stuck() {
                    self.anchor = Anchor { item: 0, skip: 0 };
                }
            }
            PageKind::Older => {
                self.older.asked = false;
                // A page that reached nothing older ends paging back.
                self.older.more = more_older && oldest.is_some();
                if oldest.is_some() {
                    self.older.position = oldest;
                }
                let k = lines.len();
                for line in lines.into_iter().rev() {
                    self.bytes += line.message.len();
                    self.entries.push_front(Entry::Line(line));
                }
                self.anchor.item += isize::try_from(k).unwrap_or(isize::MAX);
            }
            PageKind::Newer => {
                self.newer.asked = false;
                self.newer.more = more_newer && newest.is_some();
                if newest.is_some() {
                    self.newer.position = newest;
                }
                self.append(lines);
            }
        }
    }

    fn append(&mut self, lines: Vec<Line>) {
        for line in lines {
            self.bytes += line.message.len();
            self.entries.push_back(Entry::Line(line));
        }
    }

    /// The caps: the end far from the view goes.
    fn enforce_limits(&mut self) {
        while self.entries.len() > self.limits.entries || self.bytes > self.limits.bytes {
            let len = isize::try_from(self.entries.len()).unwrap_or(isize::MAX);
            let top = self.anchor.item;
            let drop_front = self.stuck() || top >= len - top;
            let gone = if drop_front {
                self.anchor.item -= 1;
                self.entries.pop_front()
            } else {
                self.entries.pop_back()
            };
            if let Some(Entry::Line(line)) = &gone {
                self.bytes -= line.message.len();
            }
            if drop_front {
                self.older.more = true;
                self.older.position = self.edge_line(true);
            } else {
                self.newer.more = true;
                self.newer.position = self.edge_line(false);
                if self.following() {
                    self.follow = Follow::Off;
                    self.source.send(Request::Follow(false));
                }
            }
        }
        if self.anchor.item < 0 && self.older.more {
            self.anchor = Anchor { item: 0, skip: 0 };
        }
    }

    /// The position of the oldest (or newest) line held.
    fn edge_line(&self, oldest: bool) -> Option<Position> {
        let position = |e: &Entry| match e {
            Entry::Line(line) => Some(line.position.clone()),
            Entry::Notice(_) => None,
        };
        if oldest {
            self.entries.iter().find_map(position)
        } else {
            self.entries.iter().rev().find_map(position)
        }
    }

    // --- items and rows ---

    fn first_item(&self) -> isize {
        if self.loaded && !self.older.more {
            -1
        } else {
            0
        }
    }

    /// One past the last item: the end marker is the last when shown.
    fn end_item(&self) -> isize {
        let len = isize::try_from(self.entries.len()).unwrap_or(isize::MAX);
        if self.end_marker().is_some() {
            len + 1
        } else {
            len
        }
    }

    fn end_marker(&self) -> Option<&'static str> {
        if !self.loaded {
            None
        } else if self.following() {
            Some("--- following ---")
        } else if !self.newer.more {
            Some("--- end of log ---")
        } else {
            None
        }
    }

    /// Item `item`'s text.
    fn item_text(&self, item: isize) -> Text<'static> {
        let muted = self.format.style(Role::Muted);
        if item < 0 {
            return Text::from(TextLine::styled("--- start of log ---", muted));
        }
        match usize::try_from(item).ok().and_then(|i| self.entries.get(i)) {
            Some(Entry::Line(line)) => line::text(line, &self.format),
            Some(Entry::Notice(notice)) => {
                let shown = DisplayText::from_bytes(notice.to_string().as_bytes(), false);
                let role = if notice.is_error() {
                    Role::Problem
                } else {
                    Role::Muted
                };
                Text::from(TextLine::styled(
                    shown.as_str().to_owned(),
                    self.format.style(role),
                ))
            }
            None => Text::from(TextLine::styled(
                self.end_marker().unwrap_or_default(),
                muted,
            )),
        }
    }

    fn paragraph(&self, item: isize) -> Paragraph<'static> {
        let paragraph = Paragraph::new(self.item_text(item));
        if self.wrap {
            paragraph.wrap(Wrap { trim: false })
        } else {
            paragraph
        }
    }

    /// Item `item`'s height on screen, at least one row.
    fn rows(&self, item: isize) -> usize {
        // ADR-001: `line_count` is ratatui's unstable rendered-line info.
        self.paragraph(item).line_count(self.width.max(1)).max(1)
    }

    /// Rows from `anchor` down, counted until `cap`.
    fn rows_below(&self, anchor: Anchor, cap: usize) -> usize {
        let mut total = 0;
        let mut item = anchor.item;
        let mut skip = anchor.skip;
        while total < cap && item < self.end_item() {
            let rows = self.rows(item);
            total += rows - skip.min(rows);
            skip = 0;
            item += 1;
        }
        total
    }

    /// Rows above `anchor`, counted until `cap`.
    fn rows_above(&self, anchor: Anchor, cap: usize) -> usize {
        let mut total = anchor.skip;
        let mut item = anchor.item;
        while total < cap && item > self.first_item() {
            item -= 1;
            total += self.rows(item);
        }
        total
    }

    /// The window's top when it shows the bottom.
    fn bottom_anchor(&self) -> Anchor {
        let mut item = self.end_item();
        let mut total = 0;
        while item > self.first_item() && total < self.height {
            item -= 1;
            total += self.rows(item);
        }
        Anchor {
            item,
            skip: total.saturating_sub(self.height),
        }
    }

    fn following(&self) -> bool {
        self.follow != Follow::Off
    }

    fn stuck(&self) -> bool {
        self.follow == Follow::Stuck
    }

    /// The window leaves the bottom: a follow pauses.
    fn unstick(&mut self) {
        if self.stuck() {
            self.follow = Follow::Paused;
        }
    }

    fn top(&self) -> Anchor {
        if self.stuck() {
            self.bottom_anchor()
        } else {
            self.anchor
        }
    }

    // --- scrolling ---

    fn scroll_up(&mut self, n: usize) {
        let mut at = self.top();
        let mut n = n;
        while n > 0 {
            if at.skip >= n {
                at.skip -= n;
                break;
            }
            n -= at.skip;
            at.skip = 0;
            if at.item <= self.first_item() {
                break;
            }
            at.item -= 1;
            at.skip = self.rows(at.item);
        }
        self.move_to(at);
    }

    /// The window's top to `at`; a follow pauses only when the window has
    /// left the bottom (a log shorter than the window doesn't move).
    fn move_to(&mut self, at: Anchor) {
        if at != self.top() {
            self.anchor = at;
            self.unstick();
        }
    }

    /// The anchor's `skip` within its item, after the item's height shrank
    /// (a wider window, `t`).
    fn clamp_anchor(&mut self) {
        let rows = self.rows(self.anchor.item);
        self.anchor.skip = self.anchor.skip.min(rows - 1);
    }

    fn scroll_down(&mut self, n: usize) {
        if self.stuck() {
            return;
        }
        let mut at = self.anchor;
        let below = self.rows_below(at, self.height + n);
        let mut n = n.min(below.saturating_sub(self.height));
        while n > 0 {
            let left = self.rows(at.item).saturating_sub(at.skip);
            if n < left {
                at.skip += n;
                break;
            }
            n -= left;
            at.item += 1;
            at.skip = 0;
        }
        self.anchor = at;
        if self.following() && self.rows_below(at, self.height + 1) <= self.height {
            self.follow = Follow::Stuck;
        }
    }

    /// End, `G`, `f`: follow again at the bottom, loading the tail first
    /// when the newest line isn't loaded.
    fn resume(&mut self) {
        if self.newer.more {
            self.source.send(Request::Tail);
            self.newer.asked = false;
        }
        if !self.following() {
            self.source.send(Request::Follow(true));
        }
        self.follow = Follow::Stuck;
    }

    /// Ask for the next page at either end within 5 screens of it.
    fn prefetch(&mut self) {
        if !self.loaded || self.height == 0 {
            return;
        }
        let reach = self.height * PREFETCH_SCREENS;
        let top = self.top();
        if self.older.more
            && !self.older.asked
            && self.rows_above(top, reach) < reach
            && let Some(edge) = self.older.position.clone()
        {
            self.source.send(Request::Older(edge));
            self.older.asked = true;
        }
        if self.newer.more
            && !self.following()
            && !self.newer.asked
            && self.rows_below(top, reach + self.height) < reach + self.height
            && let Some(edge) = self.newer.position.clone()
        {
            self.source.send(Request::Newer(edge));
            self.newer.asked = true;
        }
    }

    fn key(&mut self, key: KeyEvent) -> Flow {
        // The viewer's keys are plain (Shift only for `G`): Ctrl and Alt
        // combinations are left for later bindings.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Flow::Unchanged;
        }
        let page = self.height.max(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(1),
            KeyCode::PageUp | KeyCode::Char('u') => self.scroll_up(page),
            KeyCode::PageDown | KeyCode::Char('d') => self.scroll_down(page),
            KeyCode::Home | KeyCode::Char('g') => self.move_to(Anchor {
                item: self.first_item(),
                skip: 0,
            }),
            KeyCode::End | KeyCode::Char('G') => self.resume(),
            KeyCode::Char('f') => {
                if self.stuck() {
                    self.anchor = self.bottom_anchor();
                    self.follow = Follow::Off;
                    self.source.send(Request::Follow(false));
                } else {
                    self.resume();
                }
            }
            KeyCode::Char('w') => {
                let top = self.top();
                self.wrap = !self.wrap;
                self.anchor = Anchor {
                    item: top.item,
                    skip: 0,
                };
            }
            KeyCode::Char('t') => {
                self.format.meta = !self.format.meta;
                self.clamp_anchor();
            }
            KeyCode::Esc => {
                self.leave = Some(Leave::Back);
                return Flow::Quit;
            }
            KeyCode::Char('q') => {
                self.leave = Some(Leave::Quit);
                return Flow::Quit;
            }
            _ => return Flow::Unchanged,
        }
        self.prefetch();
        Flow::Redraw
    }

    fn header(&self) -> TextLine<'static> {
        let muted = self.format.style(Role::Muted);
        let state = match self.follow {
            Follow::Off => "",
            Follow::Stuck => "  following",
            Follow::Paused => "  following, paused",
        };
        let wrap = if self.wrap { "" } else { "  wrap off" };
        TextLine::from(vec![
            Span::styled(self.name.clone(), self.format.style(self.kind.role())),
            Span::styled(format!("  {}", self.kind.word()), muted),
            Span::styled(format!("{state}{wrap}"), self.format.style(Role::Accent)),
            Span::styled("   f follow · w wrap · t time · Esc back · q quit", muted),
        ])
    }
}

impl App for Viewer {
    fn input(&mut self, input: Input) -> Flow {
        match input {
            Input::Key(key) => self.key(key),
            _ => Flow::Unchanged,
        }
    }

    /// Takes every event the source has sent; those of another
    /// generation (a source the user left) are dropped.
    fn wake(&mut self) -> Flow {
        let mut changed = false;
        while let Ok(tagged) = self.events.try_recv() {
            if tagged.generation != self.source.generation() {
                continue;
            }
            self.apply(tagged.event);
            changed = true;
        }
        if !changed {
            return Flow::Unchanged;
        }
        self.prefetch();
        Flow::Redraw
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        if area.height == 0 {
            return;
        }
        // ratatui's layout: the header row, then the log
        // (<https://docs.rs/ratatui/0.30.2/ratatui/layout/struct.Layout.html#method.vertical>).
        let [header, log] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        frame.render_widget(Paragraph::new(self.header()), header);
        let resized = log.width != self.width || usize::from(log.height) != self.height;
        self.width = log.width;
        self.height = usize::from(log.height);
        if resized {
            self.clamp_anchor();
            self.prefetch();
        }
        let mut at = self.top();
        let mut y = 0;
        while y < log.height && at.item < self.end_item() {
            let rows = self.rows(at.item);
            let shown = u16::try_from(rows.saturating_sub(at.skip))
                .unwrap_or(u16::MAX)
                .min(log.height - y);
            let skip = u16::try_from(at.skip).unwrap_or(u16::MAX);
            let paragraph = self.paragraph(at.item).scroll((skip, 0));
            frame.render_widget(
                paragraph,
                Rect {
                    y: log.y + y,
                    height: shown,
                    ..log
                },
            );
            y += shown;
            at = Anchor {
                item: at.item + 1,
                skip: 0,
            };
        }
    }
}

#[cfg(test)]
mod tests;
