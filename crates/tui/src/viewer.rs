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
//!   and origin bytes; past either, the end far from the view is dropped,
//!   and the dropped side has more again. Dropping the newest end stops
//!   following.
//! - **Follow:** the source follows; scrolling up pauses the view (it
//!   stays put while lines come), and End, `G`, `f` or reaching the bottom
//!   resumes it. `f` while following at the bottom stops the follow.
//! - **Search** (tui spec, "Search"): `/` opens a ratatui-textarea line
//!   (<https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/struct.TextArea.html>),
//!   as the picker's filter does; Enter searches the loaded buffer,
//!   smart-case substring over each message as the screen shows it
//!   (`search`, `line::searched`), every match lit by `Format::hit`: the
//!   `SearchHit` background, or reverse video under 16 colours and with colour off.
//!   A search while following starts at the newest match, else at the
//!   first from the window's top; the view moves to it only when it isn't
//!   in sight. `n` and `N` jump to the next and previous match (no wrap at
//!   either end), the match's row at the window's top, and pause
//!   following. The header shows `/query 3/17`. Esc clears the search; a
//!   new source opens a new viewer, so the query resets.
//! - **Sideways** (wrap off): ←/→, Shift+wheel and a horizontal wheel
//!   scroll 8 columns, with no right-hand stop, as proto-logs; the header
//!   shows the column; `w` resets it. A log line is drawn cut at the
//!   column ([`columns`]), so a frame doesn't walk what lies left of it;
//!   markers and notices with `Paragraph::scroll`'s x offset.
//! - **Pasteable output** (tui spec, "Pasteable output"): `s` saves the
//!   loaded lines as plain text to a new file in the run log's directory
//!   (`save`) and shows where in the footer, with the review line, until
//!   the next key; `p` quits with [`Leave::Print`], and the caller prints
//!   [`Viewer::plain`] once the screen is gone.

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{
    Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
};
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line as TextLine, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui_textarea::{CursorMove, TextArea};
use sb_cli_core::config;
use sb_cli_core::theme::Role;
use sb_term_ui::DisplayText;
use tokio::sync::{Notify, mpsc};

use crate::ahead::{self, Ahead, Window};
use crate::columns::{self, Marks};
use crate::line::{self, Format};
use crate::resume::{self, Resumes};
use crate::save;
use crate::source::{
    Event, Generation, Line, Notice, Outbox, PageKind, Position, Request, SourceHandle, Tagged,
};
use crate::term::{App, Flow};

/// Rows the wheel scrolls (tui spec, "Scrolling").
const WHEEL_ROWS: usize = 3;

/// Columns a sideways step scrolls with wrap off (tui spec, "Scrolling").
const COLUMN_STEP: u16 = 8;

/// The search line's label and hint (the picker's filter line's layout).
const SEARCH_LABEL: &str = " Search: ";
const SEARCH_HINT: &str = "Enter searches · Esc clears ";

/// The buffer's caps (tui decision 12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Entries held.
    pub entries: usize,
    /// Message and origin bytes held.
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
    /// `p`: quit, the loaded lines printed on stdout, exit 0.
    Print,
}

/// One entry of the buffer. Not `Clone`: a copy would share its log
/// line's [`Kept`] id, and so the other's kept text.
#[derive(Debug)]
enum Entry {
    /// A log line, the matches of the search in effect in it, and what
    /// is kept with it.
    Line(Line, usize, Kept),
    /// A source notice, drawn as its own row (tui spec, "Viewer notices").
    Notice(Notice),
}

/// What a log line keeps: its id in the viewer, which [`Drawn`] knows it
/// by (an index moves as pages come and go), and its height as last
/// measured.
#[derive(Debug)]
struct Kept {
    id: u64,
    height: Cell<Option<Height>>,
}

/// A log line's height on screen and the layout it was measured at: the
/// log area's width, wrap and `t`. Kept with its entry, so a frame
/// measures only lines it hasn't measured at this layout: measuring with
/// wrap on is `Paragraph::line_count`, a wrap of the whole record (up to
/// 64 KiB), which made a frame of long records cost about twice the
/// spec's 5 ms at 240×70 when every frame measured its window again
/// (M73's frame-cost timing); with wrap off it is
/// [`line::screen_lines`]. A search's highlight splits spans but not the
/// text, so it keeps the height. Why not ratatui's own cache: its `layout-cache`
/// feature keeps `Layout::split`'s results
/// (<https://docs.rs/ratatui/0.30.2/ratatui/layout/struct.Layout.html#method.init_cache>),
/// not a paragraph's wrapped height, and ratatui has none for that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Height {
    width: u16,
    wrap: bool,
    meta: bool,
    rows: usize,
}

/// The bytes of text [`Drawn`] keeps, at most ([`Built::size`]). A
/// frame's texts are far less as a rule: with wrap off about the
/// window's width and a screen more for each row, with wrap on the few
/// records in view. 4 MiB leaves room for logs coloured word by word (a
/// span a word) and is an eighth of the buffer's 32 MiB.
const DRAWN_BYTES: usize = 4 << 20;

/// The texts of the log lines the last frame drew, for the next frame
/// to draw again without building them (M73's follow-up: building the
/// text of each record in view, `DisplayText` over up to 64 KiB, was
/// most of a frame with wrap off). Each is for the format and search it
/// was built with (the theme, the zone and `t`; the query, whose matches
/// are lit), so a change of either drops them all; a text built for
/// fewer rows or columns than a frame shows (wrap off) is built again;
/// within those, the sideways column is a cut of the text at drawing
/// ([`columns`], with the text's marks kept beside it). Far to the right
/// a text starts near the window, built from a place its line's last
/// build kept ([`resume`]): those places are kept across a change of
/// format or search too, for the next frame's texts.
///
/// Bounded to what is on screen, not counted toward the buffer's 32 MiB:
/// each frame keeps only the texts it drew, one per entry in view, so at
/// most the window's rows of them, and at most [`DRAWN_BYTES`] between
/// them (`Viewer::drawn_bytes`; a text past that is drawn and dropped,
/// as before). That is the
/// simpler design: counting them toward the cap would make the cap's
/// dropping of entries, which runs as lines come, also run as frames
/// draw, and drop log lines for texts that can be built again. Why not a
/// ratatui cache: ratatui keeps no widget's text between frames (its
/// `layout-cache` holds `Layout::split`'s results,
/// <https://docs.rs/ratatui/0.30.2/ratatui/layout/struct.Layout.html#method.init_cache>).
/// With wrap off each text is about the
/// window's columns of the rows it shows ([`line::within`]), or far to
/// the right those from a little left of the window ([`resume::far`]);
/// with wrap on it is the whole record, and every record in view but the
/// first and the last fits in the window.
///
/// A line's resume places ([`Resumes`], a few dozen bytes every 64
/// columns parsed) are kept inside the same bound: they sit in its
/// [`Built`] and count in its size, so they too are kept only for the
/// entries the last frame drew and within [`DRAWN_BYTES`]. Across a
/// change of format or search they move to `resumes`, without their
/// texts, and the next frame takes them back into the texts it builds,
/// so they are never kept beside a text of their own line, nor for a
/// line that frame doesn't draw.
#[derive(Debug, Default)]
struct Drawn {
    built_for: Option<(Format, String)>,
    texts: Vec<Built>,
    resumes: Vec<(u64, Resumes)>,
}

/// A log line's text as [`Drawn`] keeps it: whose (its [`Kept`] id), the
/// screen lines and columns it holds right (`usize::MAX`: all), the
/// screen column its lines start at (`from`, past the first far to the
/// right) and the leftmost a window can show it from (`left`), where to
/// cut it with wrap off away from the first column, once a frame has
/// needed that, and its line's resume places, once one was built far
/// right.
#[derive(Debug)]
struct Built {
    id: u64,
    text: Text<'static>,
    rows: usize,
    columns: usize,
    from: usize,
    left: usize,
    marks: Option<Marks>,
    resumes: Option<Resumes>,
}

impl Built {
    /// `text`, holding `rows` screen lines and `columns` columns right
    /// from the first.
    fn new(id: u64, text: Text<'static>, rows: usize, columns: usize) -> Self {
        Self {
            id,
            text,
            rows,
            columns,
            from: 0,
            left: 0,
            marks: None,
            resumes: None,
        }
    }

    /// `far`, a line's text built far right ([`resume::far`]), with the
    /// line's places.
    fn far(id: u64, far: resume::Far, resumes: Option<Resumes>) -> Self {
        Self {
            id,
            text: far.text,
            rows: usize::MAX,
            columns: far.columns,
            from: far.from,
            left: far.left,
            marks: None,
            resumes,
        }
    }

    /// The text's bytes, counted as its lines, their spans and the
    /// spans' contents, and its marks: what keeping it costs, less the
    /// allocator's own.
    fn size(&self) -> usize {
        ahead::text_bytes(&self.text)
            + self.marks.as_ref().map_or(0, Marks::size)
            + self.resumes.as_ref().map_or(0, Resumes::size)
    }

    /// With wrap off, the `rows` screen lines from line `skip` that a
    /// window `width` wide scrolled to `column` shows, each cut at the
    /// column ([`columns`]): ratatui then walks only what it draws, not
    /// the columns to the left (`Paragraph::scroll`'s x offset walks
    /// them all on each draw). The marks are made the first time a frame
    /// needs a cut, and again for another width, through the columns the
    /// text holds right: a window that needs more builds a new text.
    ///
    /// A text built far right ([`resume::far`]) starts a little left of
    /// the window (`from`), so it is handed over whole with the rest as
    /// `Paragraph::scroll`'s x offset (the second value), which walks only
    /// that little: as cheap as a cut, and no marks to make
    /// (<https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Paragraph.html#method.scroll>).
    fn window(&mut self, skip: usize, rows: usize, column: u16, width: u16) -> (Text<'_>, u16) {
        debug_assert!(
            usize::from(column) + usize::from(width) <= self.columns
                && self.left <= usize::from(column),
            "a window past the columns built"
        );
        let end = self.text.lines.len().min(skip.saturating_add(rows));
        let shown = skip.min(end)..end;
        let mut offset = 0;
        let lines = if column == 0 || self.from > 0 {
            let from = u16::try_from(self.from).unwrap_or(u16::MAX);
            offset = column.saturating_sub(from);
            self.text.lines[shown].iter().map(columns::whole).collect()
        } else {
            let column = usize::from(column);
            if self.marks.as_ref().map(Marks::width) != Some(width) {
                self.marks = None;
            }
            let marks = self
                .marks
                .get_or_insert_with(|| Marks::new(&self.text, width, self.columns));
            shown
                .map(|row| marks.cut(&self.text, row, column))
                .collect()
        };
        let text = Text {
            lines,
            style: self.text.style,
            alignment: self.text.alignment,
        };
        (text, offset)
    }
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
    /// The next log line's [`Kept`] id.
    next_id: u64,
    /// The texts the last frame drew, and the most bytes of them kept
    /// ([`DRAWN_BYTES`]; smaller in tests).
    drawn: Drawn,
    drawn_bytes: usize,
    /// Resume places made ahead for lines the next frames may show far
    /// right ([`crate::ahead`]).
    ahead: Ahead,
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
    /// The first column shown with wrap off.
    column: u16,
    /// The search line while it is open (`/`).
    searching: Option<TextArea<'static>>,
    /// The search in effect (empty for none), its matches in the buffer,
    /// and the one `n` and `N` move from: an entry's index and the match
    /// within it.
    query: String,
    total: usize,
    current: Option<(usize, usize)>,
    /// Where `s` saves: the run log's directory; `s` does nothing
    /// without one.
    save_dir: Option<PathBuf>,
    /// The footer's notice after `s` (its text, and whether it is an
    /// error), until the next key.
    saved: Option<(String, bool)>,
    /// Log lines measured since the count was last taken: the frame-cost
    /// test's (tui spec, Testing Strategy, "Frame cost").
    #[cfg(test)]
    measured: Cell<usize>,
    /// Log line texts built, to measure or to draw, since the count was
    /// last taken.
    #[cfg(test)]
    built: Cell<usize>,
    /// The widest screen line handed to ratatui with wrap off since the
    /// count was last taken, in columns.
    #[cfg(test)]
    widest: Cell<usize>,
    /// Message bytes parsed to build texts far right with wrap off
    /// ([`resume::far`]) since the count was last taken.
    #[cfg(test)]
    parsed: Cell<usize>,
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
        let ahead = Ahead::new(Arc::clone(&wake));
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
            next_id: 0,
            drawn: Drawn::default(),
            drawn_bytes: DRAWN_BYTES,
            ahead,
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
            column: 0,
            searching: None,
            query: String::new(),
            total: 0,
            current: None,
            save_dir: None,
            saved: None,
            #[cfg(test)]
            measured: Cell::new(0),
            #[cfg(test)]
            built: Cell::new(0),
            #[cfg(test)]
            widest: Cell::new(0),
            #[cfg(test)]
            parsed: Cell::new(0),
        }
    }

    /// `s` saves in `dir` (the run log's directory).
    #[must_use]
    pub fn saving_in(mut self, dir: PathBuf) -> Self {
        self.save_dir = Some(dir);
        self
    }

    /// The loaded lines as plain text (colour off; the time and origin as
    /// `t` shows them), and how many: what `s` saves and `p` prints. The
    /// viewer's own notices and markers aren't log lines and stay out.
    #[must_use]
    pub fn plain(&self) -> (String, usize) {
        let lines = self.entries.iter().filter_map(|entry| match entry {
            Entry::Line(line, _, _) => Some(line),
            Entry::Notice(_) => None,
        });
        save::plain(lines, &self.format)
    }

    /// `s`: the loaded lines to a new file, the footer saying where or why
    /// not; the run log gets the path and the count, never the text.
    fn save(&mut self) {
        let Some(dir) = &self.save_dir else {
            return;
        };
        let (text, count) = self.plain();
        let name = save::file_name(self.kind, &self.name, Timestamp::now());
        self.saved = Some(match save::save(dir, &name, &text) {
            Ok(path) => {
                let shown = path.display().to_string();
                tracing::info!(
                    path = %config::escape_controls(&shown),
                    lines = count,
                    "log saved"
                );
                // The buffer, not the whole log: the notice says so (spec,
                // "Pasteable output").
                let lines = if count == 1 { "line" } else { "lines" };
                let notice = format!(
                    "saved the {count} {lines} loaded in the viewer to {shown}. {}",
                    save::REVIEW
                );
                (notice, false)
            }
            Err(err) => {
                tracing::warn!(error = %err, "log not saved");
                (format!("couldn't save the log: {err}"), true)
            }
        });
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
            Event::Found { .. } => {}
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
                    self.shift_current(1);
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
            PageKind::Tail | PageKind::Around => {
                self.entries.clear();
                self.bytes = 0;
                self.total = 0;
                self.current = None;
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
                // The buffer is new: a search starts again at the newest
                // match, as a search while following does.
                if !self.query.is_empty() {
                    self.current = self.hit_before(self.entries.len());
                }
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
                    let entry = self.line_entry(line);
                    self.entries.push_front(entry);
                }
                self.anchor.item += isize::try_from(k).unwrap_or(isize::MAX);
                self.shift_current(k);
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
            let entry = self.line_entry(line);
            self.entries.push_back(entry);
        }
    }

    /// `line` as an entry, counted in the bytes and the search's matches.
    fn line_entry(&mut self, line: Line) -> Entry {
        self.bytes += line.message.len() + line.origin.len();
        let hits = if self.query.is_empty() {
            0
        } else {
            line::hits(&line, &self.format, &self.query)
        };
        self.total += hits;
        let id = self.next_id;
        self.next_id += 1;
        Entry::Line(
            line,
            hits,
            Kept {
                id,
                height: Cell::new(None),
            },
        )
    }

    /// `n` entries went in before the current match.
    fn shift_current(&mut self, n: usize) {
        if let Some((entry, _)) = &mut self.current {
            *entry += n;
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
            if let Some(Entry::Line(line, hits, _)) = &gone {
                self.bytes -= line.message.len() + line.origin.len();
                self.total -= hits;
            }
            self.current = match self.current {
                Some((0, _)) if drop_front => None,
                Some((entry, k)) if drop_front => Some((entry - 1, k)),
                Some((entry, _)) if entry >= self.entries.len() => None,
                current => current,
            };
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
            Entry::Line(line, _, _) => Some(line.position.clone()),
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
            Some(Entry::Line(line, _, _)) => {
                #[cfg(test)]
                self.built.set(self.built.get() + 1);
                line::searched(line, &self.format, &self.query, self.format.hit())
            }
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
        self.wrapped(Paragraph::new(self.item_text(item)))
    }

    /// `paragraph` wrapped when wrap is on.
    fn wrapped(&self, paragraph: Paragraph<'static>) -> Paragraph<'static> {
        if self.wrap {
            paragraph.wrap(Wrap { trim: false })
        } else {
            paragraph
        }
    }

    /// The text to draw item `item` with when it is a log line, its first
    /// `rows` screen lines shown: the last frame's from `last` when that
    /// holds as much, else built, whole with wrap on, and with wrap off
    /// as much as the window shows and a screen more to the right, so
    /// sideways steps within it build nothing ([`line::within`]). Away
    /// from the first column it is built from the resume place nearest
    /// the window that its line's last text kept ([`resume::far`]), and
    /// from the line's start only when there is none yet or the line
    /// can't be resumed.
    fn text_of(
        &self,
        item: isize,
        rows: usize,
        last: &mut Drawn,
        ahead: &mut Ahead,
    ) -> Option<Built> {
        let Some(Entry::Line(line, _, kept)) =
            usize::try_from(item).ok().and_then(|i| self.entries.get(i))
        else {
            return None;
        };
        let width = usize::from(self.width);
        // The columns the window needs right, through its right edge.
        let (rows, needed) = if self.wrap {
            (usize::MAX, usize::MAX)
        } else {
            (rows, usize::from(self.column) + width)
        };
        let column = if self.wrap {
            0
        } else {
            usize::from(self.column)
        };
        let mine = last.texts.iter().position(|b| b.id == kept.id);
        if let Some(at) = mine {
            let b = &last.texts[at];
            // A text built far right counts columns from its start: in a
            // window narrower than a character before it, ratatui would
            // have drawn that character in none.
            let fits = b.from == 0 || b.resumes.as_ref().is_some_and(|r| r.fits(self.width));
            if b.rows >= rows && b.columns >= needed && b.left <= column && fits {
                return Some(last.texts.swap_remove(at));
            }
        }
        // The resume places kept for the line, from its last text or
        // across a change of format or search; those made ahead cover
        // the whole line, so they come first.
        let kept_before = match mine {
            Some(at) => last.texts.swap_remove(at).resumes,
            None => last
                .resumes
                .iter()
                .position(|(id, _)| *id == kept.id)
                .map(|at| last.resumes.swap_remove(at).1),
        };
        let (made, text) = ahead.take(kept.id, &self.format, &self.query);
        let mut resumes = made.or(kept_before);
        if let Some(far) = text {
            // Made ahead for this format and search; used where it holds
            // the window, as a kept text is.
            let built = Built::far(kept.id, far, resumes.take());
            let fits = built.resumes.as_ref().is_some_and(|r| r.fits(self.width));
            if built.columns >= needed && built.left <= column && fits {
                return Some(built);
            }
            resumes = built.resumes;
        }
        #[cfg(test)]
        self.built.set(self.built.get() + 1);
        let hit = self.format.hit();
        if self.wrap {
            let text = line::searched(line, &self.format, &self.query, hit);
            // The line's resume places stay with it, for wrap off again.
            let mut built = Built::new(kept.id, text, rows, needed);
            built.resumes = resumes;
            return Some(built);
        }
        if column > 0 {
            // Away from the first column, from an eighth of a screen left
            // of the window through a quarter right of it: steps within
            // that (a few each way) build nothing, and a frame that
            // builds every text in view parses about a screen and a half
            // of each, wherever the column is.
            let mut places = resumes
                .take()
                .unwrap_or_else(|| Resumes::new(line, &self.format));
            let far = resume::far(
                line,
                &self.format,
                &self.query,
                hit,
                &mut places,
                self.far_window(),
                self.width,
            );
            if let Some(far) = far {
                #[cfg(test)]
                self.parsed.set(self.parsed.get() + far.parsed);
                return Some(Built::far(kept.id, far, Some(places)));
            }
            resumes = Some(places);
        }
        // A screen more to the right: the columns built for, unless
        // that is the whole message, which every window can show.
        let reach = needed + width;
        let mut built = match line::within(line, &self.format, &self.query, hit, rows, reach) {
            (text, true) => Built::new(kept.id, text, usize::MAX, usize::MAX),
            (text, false) => Built::new(kept.id, text, rows, reach),
        };
        built.resumes = resumes;
        Some(built)
    }

    /// Item `item`'s height on screen, at least one row: a log line's
    /// kept [`Height`] when it was measured at this layout. Markers and
    /// notices are one short line, measured each time.
    fn rows(&self, item: isize) -> usize {
        let line = match usize::try_from(item).ok().and_then(|i| self.entries.get(i)) {
            Some(Entry::Line(line, _, kept)) => Some((line, &kept.height)),
            _ => None,
        };
        if let Some(height) = line.and_then(|(_, kept)| kept.get())
            && (height.width, height.wrap, height.meta) == (self.width, self.wrap, self.format.meta)
        {
            return height.rows;
        }
        let rows = match line {
            // With wrap off, a line's screen lines, counted without
            // building its text (the text's lines, `line_count` without
            // a wrap).
            Some((line, _)) if !self.wrap => line::screen_lines(line, &self.format),
            // ADR-001: `line_count` is ratatui's unstable rendered-line info.
            _ => self.paragraph(item).line_count(self.width.max(1)),
        }
        .max(1);
        if let Some((_, kept)) = line {
            #[cfg(test)]
            self.measured.set(self.measured.get() + 1);
            kept.set(Some(Height {
                width: self.width,
                wrap: self.wrap,
                meta: self.format.meta,
                rows,
            }));
        }
        rows
    }

    /// Rows from `anchor` down, counted until `cap`. Every item is a row
    /// at least, so with `cap` items or more after the anchor's it is
    /// `cap` without measuring: a new layout far from the buffer's ends
    /// measures only what the window shows (M73's frame cost).
    fn rows_below(&self, anchor: Anchor, cap: usize) -> usize {
        let after = usize::try_from(self.end_item() - anchor.item - 1).unwrap_or(0);
        if after >= cap {
            return cap;
        }
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

    /// Rows above `anchor`, counted until `cap`; `cap` without measuring
    /// when the items above it and its rows above the window make that
    /// many already, as [`Self::rows_below`].
    fn rows_above(&self, anchor: Anchor, cap: usize) -> usize {
        let before = usize::try_from(anchor.item - self.first_item()).unwrap_or(0);
        if anchor.skip + before >= cap {
            return cap;
        }
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

    // --- search ---

    /// Search the loaded buffer for `query` (Enter in the search line): a
    /// search while following at the bottom starts at the newest match
    /// (paused, the view is where the user scrolled), else at the
    /// first from the window's top (or the last above it), shown only when
    /// it isn't in sight.
    fn search(&mut self, query: String) {
        self.query = query;
        self.total = 0;
        let (format, query) = (&self.format, &self.query);
        for entry in &mut self.entries {
            if let Entry::Line(line, hits, _) = entry {
                *hits = line::hits(line, format, query);
                self.total += *hits;
            }
        }
        let top = self.top_entry();
        self.current = if self.stuck() {
            self.hit_before(self.entries.len())
        } else {
            self.hit_from(top).or_else(|| self.hit_before(top))
        };
        self.show_current(false);
    }

    fn clear_search(&mut self) {
        self.query.clear();
        self.total = 0;
        self.current = None;
        for entry in &mut self.entries {
            if let Entry::Line(_, hits, _) = entry {
                *hits = 0;
            }
        }
    }

    fn hits_in(&self, entry: usize) -> usize {
        match self.entries.get(entry) {
            Some(Entry::Line(_, hits, _)) => *hits,
            _ => 0,
        }
    }

    /// The buffer's entry at the window's top (the start marker counts as
    /// the first).
    fn top_entry(&self) -> usize {
        usize::try_from(self.top().item).unwrap_or(0)
    }

    /// The first match in entry `from` or after it.
    fn hit_from(&self, from: usize) -> Option<(usize, usize)> {
        (from..self.entries.len())
            .find(|&e| self.hits_in(e) > 0)
            .map(|e| (e, 0))
    }

    /// The last match before entry `to`.
    fn hit_before(&self, to: usize) -> Option<(usize, usize)> {
        (0..to.min(self.entries.len()))
            .rev()
            .find(|&e| self.hits_in(e) > 0)
            .map(|e| (e, self.hits_in(e) - 1))
    }

    /// `n` (`forward`) and `N`: the next or previous match, from the
    /// current one, else from the window's top; none past either end.
    fn jump(&mut self, forward: bool) {
        if self.query.is_empty() {
            return;
        }
        let next = match (self.current, forward) {
            (Some((e, k)), true) if k + 1 < self.hits_in(e) => Some((e, k + 1)),
            (Some((e, _)), true) => self.hit_from(e + 1),
            (Some((e, k)), false) if k > 0 => Some((e, k - 1)),
            (Some((e, _)), false) => self.hit_before(e),
            (None, true) => self.hit_from(self.top_entry()),
            (None, false) => self.hit_before(self.top_entry() + 1),
        };
        if next.is_some() {
            // While following, the window's top is worked out from the
            // bottom: pin it first, so a match already on the top row
            // leaves the view where it is when following pauses.
            if self.stuck() {
                self.anchor = self.bottom_anchor();
            }
            self.current = next;
            self.show_current(true);
            self.unstick();
        }
    }

    /// Move the view to the current match, its row at the window's top:
    /// always for `n` and `N` (`jump`), else only when it is out of sight.
    /// With wrap off, the column moves too when the match is off to a side.
    ///
    /// The match's row in a wrapped entry is the height of the text drawn
    /// up to the end of the word the match starts in, less the rows that
    /// word's rest takes, measured with `Paragraph::line_count` (ADR-001),
    /// as the entries' heights are. UNVERIFIED: ratatui has no API that
    /// gives the row a character lands on; checked against ratatui's word
    /// wrap in the viewer's tests (a match at a row's start, inside a
    /// wrapped entry). A word wider than the window, which the wrap breaks
    /// anywhere, can put the match a row off.
    fn show_current(&mut self, jump: bool) {
        let Some((entry, k)) = self.current else {
            return;
        };
        let Some(Entry::Line(line, _, _)) = self.entries.get(entry) else {
            return;
        };
        let Some((screen_line, lead, word)) = line::hit_lead(line, &self.format, &self.query, k)
        else {
            return;
        };
        let item = isize::try_from(entry).unwrap_or(isize::MAX);
        let width = self.width.max(1);
        let skip = if self.wrap {
            let text = self.item_text(item);
            let above = Paragraph::new(Text::from(text.lines[..screen_line].to_vec()))
                .wrap(Wrap { trim: false })
                .line_count(width);
            // The rows up to the end of the match's word, less the rows
            // the rest of the word takes.
            let through = Paragraph::new(format!("{lead}{word}"))
                .wrap(Wrap { trim: false })
                .line_count(width)
                .max(1);
            let word_rows = Span::raw(word.as_str())
                .width()
                .div_ceil(usize::from(width))
                .max(1);
            above + through.saturating_sub(word_rows)
        } else {
            screen_line
        };
        if !self.wrap {
            // Off to a side, the match goes to the window's middle (in
            // steps of 8), so the text around it shows too.
            let at = u16::try_from(Span::raw(lead.as_str()).width()).unwrap_or(u16::MAX);
            if at < self.column || at >= self.column.saturating_add(width) {
                let left = at.saturating_sub(width / 2);
                self.column = left - left % COLUMN_STEP;
            }
        }
        let at = Anchor { item, skip };
        if jump || !self.in_sight(at) {
            self.move_to(at);
        }
    }

    /// Whether the row `at` is in the window.
    fn in_sight(&self, at: Anchor) -> bool {
        let top = self.top();
        if (at.item, at.skip) < (top.item, top.skip) {
            return false;
        }
        let mut rows = 0;
        let mut item = top.item;
        let mut skip = top.skip;
        while item < at.item {
            rows += self.rows(item).saturating_sub(skip);
            skip = 0;
            item += 1;
            if rows >= self.height {
                return false;
            }
        }
        rows + at.skip.saturating_sub(skip) < self.height
    }

    /// The search line open: Enter searches (an empty line clears the
    /// search), Esc closes it and clears the search, and every other key
    /// is the line's (ratatui-textarea's `TextArea::input`). Enter and
    /// Ctrl-M, which the line would take as a new line, end it, as
    /// ratatui-textarea's single-line example does
    /// (<https://github.com/ratatui/ratatui-textarea/blob/v0.9.3/examples/single_line.rs>).
    fn search_key(&mut self, key: KeyEvent) -> Flow {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter | KeyCode::Char('\n' | '\r') => self.end_search_line(),
            KeyCode::Char('m') if ctrl => self.end_search_line(),
            KeyCode::Esc => {
                self.searching = None;
                self.clear_search();
            }
            _ => {
                let Some(line) = self.searching.as_mut() else {
                    return Flow::Unchanged;
                };
                line.input(key);
            }
        }
        self.prefetch();
        Flow::Redraw
    }

    /// A paste (bracketed paste): into the search line whole, as one line
    /// (ratatui-textarea's `insert_str`), the line left open for Enter;
    /// outside the line it does nothing, so its letters aren't keys.
    fn paste(&mut self, text: &str) -> Flow {
        let Some(line) = self.searching.as_mut() else {
            return Flow::Unchanged;
        };
        line.insert_str(crate::paste::one_line(text));
        Flow::Redraw
    }

    fn end_search_line(&mut self) {
        let query = self
            .searching
            .take()
            .map_or_default(|line| line.lines().join(""));
        if query.is_empty() {
            self.clear_search();
        } else {
            self.search(query);
        }
    }

    /// One sideways step with wrap off; nothing with wrap on.
    fn sideways(&mut self, right: bool) -> bool {
        if self.wrap {
            return false;
        }
        self.column = if right {
            self.column.saturating_add(COLUMN_STEP)
        } else {
            self.column.saturating_sub(COLUMN_STEP)
        };
        true
    }

    fn key(&mut self, key: KeyEvent) -> Flow {
        if self.searching.is_some() {
            return self.search_key(key);
        }
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
                self.column = 0;
                self.anchor = Anchor {
                    item: top.item,
                    skip: 0,
                };
            }
            KeyCode::Left | KeyCode::Right => {
                if !self.sideways(key.code == KeyCode::Right) {
                    return Flow::Unchanged;
                }
            }
            KeyCode::Char('/') => {
                // The line opens on the search in effect, the cursor at
                // its end (ratatui-textarea's `TextArea::new` and
                // `move_cursor`), as the picker's filter line does.
                let mut line = TextArea::new(vec![self.query.clone()]);
                line.move_cursor(CursorMove::End);
                line.set_cursor_line_style(Style::default());
                line.set_style(self.format.style(Role::Text));
                self.searching = Some(line);
            }
            KeyCode::Char('n') => self.jump(true),
            KeyCode::Char('N') => self.jump(false),
            KeyCode::Esc if !self.query.is_empty() => self.clear_search(),
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
            KeyCode::Char('p') => {
                self.leave = Some(Leave::Print);
                return Flow::Quit;
            }
            KeyCode::Char('s') => self.save(),
            _ => return Flow::Unchanged,
        }
        self.prefetch();
        Flow::Redraw
    }

    /// The wheel scrolls as ↑ and ↓ do, 3 rows at a time (crossterm's
    /// `MouseEventKind::ScrollUp`/`ScrollDown`,
    /// <https://docs.rs/crossterm/0.29.0/crossterm/event/enum.MouseEventKind.html>).
    /// With wrap off, Shift+wheel and the horizontal wheel
    /// (`ScrollLeft`/`ScrollRight`) step sideways, as ←/→ do (proto-logs).
    fn mouse(&mut self, mouse: MouseEvent) -> Flow {
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let sideways = match mouse.kind {
            MouseEventKind::ScrollLeft => Some(false),
            MouseEventKind::ScrollRight => Some(true),
            MouseEventKind::ScrollUp if shift && !self.wrap => Some(false),
            MouseEventKind::ScrollDown if shift && !self.wrap => Some(true),
            _ => None,
        };
        match (sideways, mouse.kind) {
            (Some(right), _) => {
                if !self.sideways(right) {
                    return Flow::Unchanged;
                }
            }
            (None, MouseEventKind::ScrollUp) => self.scroll_up(WHEEL_ROWS),
            (None, MouseEventKind::ScrollDown) => self.scroll_down(WHEEL_ROWS),
            _ => return Flow::Unchanged,
        }
        self.prefetch();
        Flow::Redraw
    }

    /// The log area: each item from the window's top down, log lines
    /// from the last frame's texts where it kept them ([`Drawn`]).
    fn draw_log(&mut self, frame: &mut Frame<'_>, log: Rect) {
        let resized = log.width != self.width || usize::from(log.height) != self.height;
        self.width = log.width;
        self.height = usize::from(log.height);
        if resized {
            self.clamp_anchor();
            self.prefetch();
        }
        let mut last = std::mem::take(&mut self.drawn);
        let mut ahead = std::mem::take(&mut self.ahead);
        let built_for = (self.format.clone(), self.query.clone());
        if last.built_for.as_ref() != Some(&built_for) {
            // The texts go, their resume places stay (they hold no
            // colour of the profile, no search and no time).
            last.resumes = last
                .texts
                .drain(..)
                .filter_map(|b| Some((b.id, b.resumes?)))
                .collect();
        }
        let mut drawn = Drawn {
            built_for: Some(built_for),
            texts: Vec::new(),
            resumes: Vec::new(),
        };
        let mut kept = 0;
        let mut at = self.top();
        let mut y = 0;
        while y < log.height && at.item < self.end_item() {
            let rows = self.rows(at.item);
            let shown = u16::try_from(rows.saturating_sub(at.skip))
                .unwrap_or(u16::MAX)
                .min(log.height - y);
            let area = Rect {
                y: log.y + y,
                height: shown,
                ..log
            };
            if let Some(mut built) =
                self.text_of(at.item, at.skip + usize::from(shown), &mut last, &mut ahead)
            {
                if self.wrap {
                    let skip = u16::try_from(at.skip).unwrap_or(u16::MAX);
                    let paragraph = self.wrapped(Paragraph::new(built.text.clone()));
                    frame.render_widget(paragraph.scroll((skip, 0)), area);
                } else {
                    // With wrap off, the window's rows of the text, cut
                    // at the sideways column.
                    let rows = usize::from(shown);
                    let (text, offset) = built.window(at.skip, rows, self.column, log.width);
                    #[cfg(test)]
                    {
                        let widest = text.lines.iter().map(TextLine::width).max();
                        self.widest.set(self.widest.get().max(widest.unwrap_or(0)));
                    }
                    frame.render_widget(Paragraph::new(text).scroll((0, offset)), area);
                }
                let size = built.size();
                if kept + size <= self.drawn_bytes {
                    kept += size;
                    drawn.texts.push(built);
                }
            } else {
                // A marker or a notice, a row: with wrap off, the x
                // offset is the sideways column
                // (<https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Paragraph.html#method.scroll>).
                let skip = u16::try_from(at.skip).unwrap_or(u16::MAX);
                let column = if self.wrap { 0 } else { self.column };
                frame.render_widget(self.paragraph(at.item).scroll((skip, column)), area);
            }
            y += shown;
            at = Anchor {
                item: at.item + 1,
                skip: 0,
            };
        }
        self.drawn = drawn;
        self.ahead = ahead;
        self.want_ahead();
    }

    /// With wrap off, ask for the resume places of the lines the next
    /// frames may show far right, nearest first: the window's, a screen
    /// either way, the buffer's two ends (Home, End), the lines around
    /// the search's next and previous match (`n`, `N`), then a second
    /// screen either way. Away from the first column, the texts too, as
    /// `text_of` would build them now, for the lines off screen that a
    /// key brings in at once (a screen either way, the ends); the
    /// window's own are drawn already. A line that fits in
    /// the window needs nothing. With wrap on nothing is asked for, and
    /// what was kept goes.
    fn want_ahead(&mut self) {
        let mut entries: Vec<(usize, bool)> = Vec::new();
        if !self.wrap {
            let len = self.entries.len();
            let screen = self.height.max(1);
            let top = self.top_entry();
            let mut add = |from: usize, to: usize, text: bool| {
                entries.extend((from.min(len)..to.min(len)).map(|entry| (entry, text)));
            };
            add(top, top + screen, false);
            add(top + screen, top + 2 * screen, true);
            add(top.saturating_sub(screen), top, true);
            add(0, screen, true);
            add(len.saturating_sub(screen), len, true);
            if !self.query.is_empty() {
                let next = self.hit_from(top + screen).map(|(entry, _)| entry);
                let before = self.hit_before(top).map(|(entry, _)| entry);
                // Places only: `n` and `N` move the column to the match
                // when it is out of sight, so a text for this window
                // wouldn't serve.
                for entry in next.into_iter().chain(before) {
                    add(entry.saturating_sub(screen / 2), entry + screen, false);
                }
            }
            add(top + 2 * screen, top + 3 * screen, false);
            add(
                top.saturating_sub(2 * screen),
                top.saturating_sub(screen),
                false,
            );
        }
        let mut seen = std::collections::HashSet::new();
        let mut ids = Vec::new();
        let mut lines = std::collections::HashMap::new();
        for (entry, text) in entries {
            if let Some(Entry::Line(line, _, kept)) = self.entries.get(entry)
                && seen.insert(entry)
            {
                ids.push((kept.id, text));
                lines.insert(kept.id, line);
            }
        }
        let window = (!self.wrap && self.column > 0).then(|| {
            let (left, reach) = self.far_window();
            Window {
                format: self.format.clone(),
                query: self.query.clone(),
                left,
                reach,
                column: usize::from(self.column),
                width: self.width,
            }
        });
        let (format, width) = (&self.format, usize::from(self.width));
        self.ahead.want(&ids, format, window, |id| {
            let line = *lines.get(&id)?;
            let shown = line::shown(line, format).0;
            // Fits in the window (a tab is the only byte wider than a
            // column): never built far right.
            if shown.len() <= width && !shown.contains(&b'\t') {
                return None;
            }
            Some(line)
        });
    }

    /// Wait until everything wanted is made ahead (tests: as the time to a
    /// person's next key lets the worker finish).
    #[cfg(test)]
    fn settle_ahead(&mut self) {
        loop {
            self.ahead.settle();
            if !self.ahead.wants_more() {
                return;
            }
            self.want_ahead();
        }
    }

    /// The columns a text built far right holds ([`resume::far`]'s
    /// `left` and `reach`): from an eighth of a screen left of the window
    /// through a quarter right of it.
    fn far_window(&self) -> (usize, usize) {
        let (column, width) = (usize::from(self.column), usize::from(self.width));
        (column.saturating_sub(width / 8), column + width + width / 4)
    }

    fn header(&self) -> TextLine<'static> {
        let muted = self.format.style(Role::Muted);
        let state = match self.follow {
            Follow::Off => "",
            Follow::Stuck => "  following",
            Follow::Paused => "  following, paused",
        };
        let wrap = if self.wrap {
            String::new()
        } else {
            format!("  wrap off · col {}", u32::from(self.column) + 1)
        };
        // The search's keys lead the hints while a search is in effect.
        let keys = if self.query.is_empty() {
            "/ search · f follow · w wrap · t time · Esc back · q quit · s save · p print"
        } else {
            "n/N next · Esc clear · f follow · w wrap · t time · q quit · s save · p print"
        };
        TextLine::from(vec![
            Span::styled(self.name.clone(), self.format.style(self.kind.role())),
            Span::styled(format!("  {}", self.kind.word()), muted),
            Span::styled(format!("{state}{wrap}"), self.format.style(Role::Accent)),
            Span::styled(self.search_state(), self.format.style(Role::Highlight)),
            Span::styled(format!("   {keys}"), muted),
        ])
    }

    /// The header's search part: `  /query 3/17`, `-/17` when the current
    /// match was dropped from the buffer, `no matches`; nothing without a
    /// search. The query is the user's, through `DisplayText` all the same.
    fn search_state(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        let query = DisplayText::from_bytes(self.query.as_bytes(), false);
        let query = query.lines().next().unwrap_or_default();
        if self.total == 0 {
            return format!("  /{query} no matches");
        }
        match self.current {
            Some((entry, k)) => {
                let before: usize = (0..entry).map(|e| self.hits_in(e)).sum();
                format!("  /{query} {}/{}", before + k + 1, self.total)
            }
            None => format!("  /{query} -/{}", self.total),
        }
    }
}

impl App for Viewer {
    fn input(&mut self, input: Input) -> Flow {
        match input {
            Input::Key(key) => {
                // The save's notice goes with the next key.
                let cleared = self.saved.take().is_some();
                match self.key(key) {
                    Flow::Unchanged if cleared => Flow::Redraw,
                    flow => flow,
                }
            }
            Input::Mouse(mouse) => self.mouse(mouse),
            Input::Paste(text) => self.paste(&text),
            _ => Flow::Unchanged,
        }
    }

    /// Takes every event the source has sent; those of another
    /// generation (a source the user left) are dropped.
    fn wake(&mut self) -> Flow {
        // What was made ahead changes nothing on screen: taken in, no
        // frame; lines not yet asked for are asked for now.
        self.ahead.drain();
        if self.ahead.wants_more() {
            self.want_ahead();
        }
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
        // The footer: the search line, else the save's notice, wrapped
        // (`line_count`, ADR-001) and kept to a third of the screen.
        let notice = self.saved.as_ref().map(|(text, error)| {
            let shown = DisplayText::from_bytes(text.as_bytes(), false);
            let role = if *error { Role::Problem } else { Role::Muted };
            Paragraph::new(Span::styled(
                shown.as_str().to_owned(),
                self.format.style(role),
            ))
            .wrap(Wrap { trim: false })
        });
        let footer_rows = match (&self.searching, &notice) {
            (Some(_), _) => 1,
            (None, Some(notice)) => {
                let rows = notice.line_count(area.width.max(1));
                u16::try_from(rows).unwrap_or(u16::MAX).min(area.height / 3)
            }
            (None, None) => 0,
        };
        let [header, log, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(footer_rows),
        ])
        .areas(area);
        frame.render_widget(Paragraph::new(self.header()), header);
        if let (None, Some(notice)) = (&self.searching, notice) {
            frame.render_widget(notice, footer);
        }
        if let Some(line) = &self.searching {
            let width = |text| u16::try_from(Span::raw(text).width()).unwrap_or(u16::MAX);
            let [label_area, line_area, hint_area] = Layout::horizontal([
                Constraint::Length(width(SEARCH_LABEL)),
                Constraint::Fill(1),
                Constraint::Length(width(SEARCH_HINT)),
            ])
            .areas(footer);
            frame.render_widget(
                Paragraph::new(Span::styled(SEARCH_LABEL, self.format.style(Role::Accent))),
                label_area,
            );
            frame.render_widget(
                Paragraph::new(Span::styled(SEARCH_HINT, self.format.style(Role::Muted))),
                hint_area,
            );
            // ratatui-textarea draws itself as a widget
            // (<https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/struct.TextArea.html#impl-Widget-for-%26TextArea%3C'_%3E>).
            frame.render_widget(line, line_area);
        }
        self.draw_log(frame, log);
    }
}

#[cfg(test)]
mod tests;
