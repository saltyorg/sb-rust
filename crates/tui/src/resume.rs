//! A wrap-off line built from near the column the window shows, not from
//! its start (the viewer far to the right, the tui spec's success
//! criterion 8; the follow-up "The viewer's far-right rebuild frame meets
//! 5 ms").
//!
//! A frame that builds the texts in view far to the right again (a
//! search, a theme or `t` change, a resize there) parsed every message
//! from its first byte for its styles: `DisplayText` and ansi-to-tui
//! read in order and neither can start partway through a line, so at
//! column 4,000 a frame of 70 records took 6 ms, and 19 ms on CJK or
//! colour-per-word records. Here each line keeps [`Resumes`]: places,
//! about every [`EVERY`] columns, where the parse can start again, each
//! with the message's byte there, `DisplayText`'s column (for its tab
//! stops), the screen column and the style in effect. A build starts at
//! the last one left of the window and parses only from there.
//!
//! - `DisplayText::resumable` (term-ui) gives the places: before a
//!   printed character, so its parser (anstyle-parse's DEC machine) is in
//!   its ground state, with no carriage return pending.
//! - ansi-to-tui's state between spans is one `Style`, and each SGR
//!   changes it by setting colours, adding or taking modifiers, or
//!   resetting it whole (its `parser.rs`, `line` and `AnsiStates`), so a
//!   parse started with no style gives each span the style that,
//!   patched over the style in effect where it started, is the whole
//!   parse's (`Style::patch`,
//!   <https://docs.rs/ratatui/0.30.2/ratatui/style/struct.Style.html#method.patch>).
//!   ansi-to-tui can't start from a style itself
//!   (<https://docs.rs/ansi-to-tui/8.0.1/ansi_to_tui/trait.IntoText.html>).
//!   UNVERIFIED against its docs, which don't describe the state: checked
//!   by drawing both texts in `a_resumed_line_draws_as_the_whole_text`
//!   (resets whole and partial, colours of every form, modifiers on and
//!   off, searches, tabs, wide and combining characters).
//! - A place is kept only at a grapheme's start, as ratatui's
//!   `Span::styled_graphemes` splits the text
//!   (<https://docs.rs/ratatui/0.30.2/ratatui/text/struct.Span.html#method.styled_graphemes>),
//!   and grapheme breaks after a break don't depend on what came before
//!   it (UAX #29, <https://www.unicode.org/reports/tr29/>), so the text
//!   from there draws as the whole text does from there; its screen
//!   column is the cells of the graphemes before it (`CellWidth`).
//!
//! Only a message of one screen line with no carriage return that takes
//! it back is built so; any other is built from its start
//! ([`crate::line::within`]), as is a search for a query that can overlap
//! itself (`aa`, `abab`): its matches from a place partway through may
//! pair up differently than from the line's start; and a search on a line
//! with a character that joins the one before it ([`Resumes`]' `joins`).

use ansi_to_tui::IntoText as _;
use ratatui::buffer::CellWidth as _;
use ratatui::style::Style;
use ratatui::text::{Line as TextLine, Span, Text};
use sb_cli_core::theme::Role;
use sb_term_ui::DisplayText;

use crate::columns::offset;
use crate::line::{self, Format};
use crate::search;
use crate::source::Line;

/// The columns between a line's resumes, about: a build starts about
/// this far left of where it has to (further only where no printable
/// ASCII is near). Why 64: a build far right then parses about 400
/// columns at 240, a sixth of them for this; a resume costs at most 48
/// bytes, under a byte a column of the text it saves parsing.
pub(crate) const EVERY: usize = 64;

/// Where a parse of a line's message can start again.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    /// The message's byte there.
    byte: usize,
    /// The screen column of the message there, from the message's start.
    column: usize,
    /// `DisplayText`'s column there, for its tab stops.
    tab: usize,
    /// ansi-to-tui's style in effect, before the profile's colours.
    style: Style,
}

/// A log line's [`Point`]s, in order, as far as builds of it have
/// parsed; kept with its text in the viewer's frame cache, counted in
/// its bytes.
#[derive(Clone, Debug)]
pub(crate) struct Resumes {
    /// The message is one screen line that no carriage return takes back.
    usable: bool,
    /// The colour the points were made with: with it off, `DisplayText`
    /// keeps no SGR and every style is empty.
    colour: bool,
    /// The bytes of the message shown they were made for (the format's
    /// record cap).
    shown: usize,
    /// The message holds a character that joins the one before it into
    /// a grapheme wider than the two apart (a variation selector, a zero
    /// width joiner, a keycap's mark, a skin tone, a tag): a search match
    /// that ends between them splits the grapheme, as ratatui measures
    /// each span apart, and moves the columns after it from what the
    /// points say.
    joins: bool,
    /// The widest grapheme met, in cells: a window narrower than that
    /// draws it in none, which the points' columns don't count.
    widest: u16,
    points: Vec<Point>,
}

impl Resumes {
    /// No points yet for `line`, and whether it can have any.
    pub(crate) fn new(line: &Line, format: &Format) -> Self {
        let shown = line::shown(line, format).0;
        // `contains` looks for a byte as memchr does, faster than a scan
        // of pairs: most messages hold no carriage return to check.
        let newline = shown
            .split_last()
            .is_some_and(|(_, before)| before.contains(&b'\n'));
        let back = shown.contains(&b'\r') && line::takes_back(shown, 0);
        Self {
            usable: !newline && !back,
            colour: format.colour(),
            shown: shown.len(),
            joins: !shown.is_ascii() && (0..shown.len()).any(|at| joins(&shown[at..])),
            widest: 1,
            points: Vec::new(),
        }
    }

    /// The points' bytes: what keeping them costs, less the allocator's
    /// own.
    pub(crate) fn size(&self) -> usize {
        size_of::<Self>() + self.points.capacity() * size_of::<Point>()
    }

    /// About the bytes `columns` columns of the message take: as many as
    /// they take up to its last point, an eighth more and 16 (a few
    /// escape sequences' worth, for short spans), else twice the columns
    /// (as [`crate::line::within`] starts). A parse that falls short is
    /// done again twice as long.
    fn bytes_for(&self, columns: usize) -> usize {
        match self.points.last() {
            Some(last) if last.column > 0 => {
                columns.saturating_mul(last.byte) / last.column + columns / 8 + 16
            }
            _ => columns.saturating_mul(2),
        }
    }

    /// Whether a window `width` wide draws every grapheme met in a cell
    /// or more, as the points' columns count them.
    pub(crate) fn fits(&self, width: u16) -> bool {
        self.widest <= width
    }

    /// The last point at or left of message column `column`.
    fn before(&self, column: usize) -> Option<Point> {
        let at = self.points.partition_point(|p| p.column <= column);
        at.checked_sub(1).map(|at| self.points[at])
    }

    /// `found`, the points of a parse from `from` that reached message
    /// byte `end`, in place of those it passed.
    fn merge(&mut self, from: Option<Point>, found: &[(Point, usize)], end: usize) {
        let start = from.map_or(0, |p| p.byte);
        let after = self.points.partition_point(|p| p.byte < end);
        let before = self.points.partition_point(|p| p.byte <= start);
        let later = self.points.split_off(after);
        self.points.truncate(before);
        self.points
            .extend(found.iter().map(|&(p, _)| p).filter(|p| p.byte > start));
        self.points.extend(later);
    }
}

/// A line's text as a window from column `left` through `reach` shows it
/// ([`far`]).
#[derive(Debug)]
pub(crate) struct Far {
    /// One screen line, from screen column `from`.
    pub(crate) text: Text<'static>,
    pub(crate) from: usize,
    /// The leftmost column a window can start at and draw it right: past
    /// `from` by a search match's width, whose start may be left of it.
    pub(crate) left: usize,
    /// The columns it holds right (`usize::MAX`: through the end).
    pub(crate) columns: usize,
    /// The message's bytes parsed to build it (the frame-cost test's
    /// count).
    #[cfg(test)]
    pub(crate) parsed: usize,
}

/// One parse of a message from a point.
struct Run {
    /// The line it made, styled as the whole parse styles it, before the
    /// profile's colours.
    line: TextLine<'static>,
    /// The points it found after its start, each with its byte in the
    /// line's text.
    points: Vec<(Point, usize)>,
    /// Its line's columns.
    columns: usize,
    widest: u16,
}

/// `line` as a wrap-off window from screen column `left` through
/// `reach` shows it, built from the last point left of `left` (less a
/// match's width for a search) and the points found on the way kept in
/// `resumes`; `None` where it has to be built from its start
/// ([`crate::line::within`]): a message of several lines, a carriage
/// return that takes it back, a query that can overlap itself, a window
/// narrower than a grapheme in it. A window `width` wide.
pub(crate) fn far(
    line: &Line,
    format: &Format,
    query: &str,
    hit: Style,
    resumes: &mut Resumes,
    (left, reach): (usize, usize),
    width: u16,
) -> Option<Far> {
    let (shown, cut) = line::shown(line, format);
    if resumes.colour != format.colour() || resumes.shown != shown.len() {
        *resumes = Resumes::new(line, format);
    }
    let lowered = query.to_lowercase();
    let split = !query.is_empty() && resumes.joins;
    if !resumes.usable || !resumes.fits(width) || split || overlaps(query, &lowered) {
        return None;
    }
    let meta = if format.meta {
        line::meta(line, format)
    } else {
        String::new()
    };
    let (meta_columns, meta_widest) = cells(&meta);
    if meta_widest > width {
        return None;
    }
    // A match that a window from `left` shows part of starts at most its
    // width before it: two columns a character.
    let margin = 2 * lowered.chars().count();
    let target = left.saturating_sub(margin).checked_sub(meta_columns);
    let from = target.and_then(|column| resumes.before(column));
    // As `within`: past the window by a match's width, and two columns
    // for a character the parse's end splits.
    let need = (reach + margin + 2).saturating_sub(meta_columns + from.map_or(0, |p| p.column));
    let start = from.map_or(0, |p| p.byte);
    let mut end = start
        .saturating_add(resumes.bytes_for(need))
        .min(shown.len());
    #[cfg(test)]
    let mut parsed = 0;
    let run = loop {
        let run = run(shown, from, end, format.colour())?;
        #[cfg(test)]
        {
            parsed += end - start;
        }
        if run.columns >= need || end == shown.len() {
            break run;
        }
        end = start
            .saturating_add((end - start).saturating_mul(2))
            .min(shown.len());
    };
    resumes.widest = resumes.widest.max(run.widest);
    resumes.merge(from, &run.points, end);
    if !resumes.fits(width) {
        return None;
    }
    // Cut at the last point found at or left of the target, if any is
    // nearer it than the parse's start.
    let cut_at = target.and_then(|column| {
        run.points
            .iter()
            .rev()
            .find(|(p, _)| p.column <= column)
            .copied()
    });
    let mut text_line = run.line;
    // And at the first point past what the window needs: a parse made
    // longer to be sure of filling it (twice the columns in bytes when no
    // points tell how many a column takes) can reach far past it.
    let needed_end = (reach + margin + 2).saturating_sub(meta_columns);
    let past = run.points.iter().find(|(p, _)| p.column >= needed_end);
    if let Some(&(_, at)) = past {
        keep_before(&mut text_line, at);
    }
    let start_column = match cut_at {
        Some((point, at)) => {
            drop_before(&mut text_line, at);
            Some(point.column)
        }
        None => from.map(|p| p.column),
    };
    line::lossy_line(&mut text_line, format.theme.profile());
    if !query.is_empty() && hit != Style::new() {
        search::highlight(&mut text_line, query, hit);
    }
    let whole = end == shown.len() && past.is_none();
    if whole && cut > 0 {
        text_line.spans.push(Span::styled(
            format!(" … [{cut} bytes not shown]"),
            format.style(Role::Muted),
        ));
    }
    let from_column = if let Some(column) = start_column {
        meta_columns + column
    } else {
        if !meta.is_empty() {
            text_line.spans.insert(0, Span::raw(meta));
        }
        0
    };
    Some(Far {
        text: Text::from(text_line),
        from: from_column,
        left: if from_column == 0 {
            0
        } else {
            from_column + margin
        },
        columns: if whole { usize::MAX } else { reach },
        #[cfg(test)]
        parsed,
    })
}

/// Parse `shown` from `from` (its start: `None`) to byte `end`.
fn run(shown: &[u8], from: Option<Point>, end: usize, colour: bool) -> Option<Run> {
    let start = from.unwrap_or(Point {
        byte: 0,
        column: 0,
        tab: 0,
        style: Style::new(),
    });
    let (display, places) =
        DisplayText::resumable(&shown[start.byte..end], colour, start.tab, EVERY);
    let mut line = if colour {
        // `DisplayText` keeps only SGR, which ansi-to-tui parses; should it
        // refuse the text, the line is built from its start.
        display.as_str().into_text().ok()?.lines.into_iter().next()
    } else {
        display.lines().next().map(|l| TextLine::raw(l.to_owned()))
    }
    .unwrap_or_default();
    for span in &mut line.spans {
        span.style = start.style.patch(span.style);
    }
    // Walk the line's graphemes for each place's column, keeping those
    // at a grapheme's start.
    let mut places = places.into_iter().filter(|p| p.line == 0).peekable();
    let mut points = Vec::new();
    let mut column = start.column;
    let mut seen = 0;
    let mut widest = 1;
    // Keeps the place at `visible`, at `column`, if a place is there; any
    // before it fell inside a grapheme.
    let mut keep = |visible: usize, column: usize, style: Style| {
        while places.next_if(|p| p.visible < visible).is_some() {}
        if let Some(place) = places.next_if(|p| p.visible == visible) {
            let point = Point {
                byte: start.byte + place.byte,
                column,
                tab: place.column,
                style,
            };
            points.push((point, visible));
        }
        places.peek().map(|p| p.visible)
    };
    for span in &line.spans {
        let content: &str = &span.content;
        if content.bytes().all(|b| matches!(b, b' '..=b'~')) {
            // Printable ASCII: each byte a grapheme of one column, so only
            // the places in it are visited.
            let mut next = keep(seen, column, span.style);
            while let Some(visible) = next.filter(|&v| v < seen + content.len()) {
                next = keep(visible, column + visible - seen, span.style);
            }
            column += content.len();
        } else {
            for grapheme in span.styled_graphemes(Style::new()) {
                keep(seen + offset(content, grapheme.symbol), column, span.style);
                let cells = grapheme.symbol.cell_width();
                widest = widest.max(cells);
                column += usize::from(cells);
            }
        }
        seen += content.len();
    }
    Some(Run {
        line,
        points,
        columns: column - start.column,
        widest,
    })
}

/// Whether `bytes` start with a character that joins the one before it
/// into a grapheme whose width isn't the sum of theirs (see
/// [`Resumes`]): U+FE00 to U+FE0F, U+200D, U+20E3, U+1F3FB to U+1F3FF
/// and the tags U+E0000 to U+E007F, in UTF-8.
fn joins(bytes: &[u8]) -> bool {
    matches!(
        bytes,
        [0xef, 0xb8, 0x80..=0x8f, ..]
            | [0xe2, 0x80, 0x8d, ..]
            | [0xe2, 0x83, 0xa3, ..]
            | [0xf0, 0x9f, 0x8f, 0xbb..=0xbf, ..]
            | [0xf3, 0xa0, 0x80 | 0x81, ..]
    )
}

/// Take what comes before byte `at` of `line`'s text off it. Why not a
/// ratatui call: ratatui-core has no public cut of a `Line` (see
/// `columns.rs`).
fn drop_before(line: &mut TextLine<'static>, at: usize) {
    let mut seen = 0;
    let first = line.spans.iter().position(|span| {
        seen += span.content.len();
        seen > at
    });
    let Some(first) = first else {
        line.spans.clear();
        return;
    };
    line.spans.drain(..first);
    let skip = at - (seen - line.spans[0].content.len());
    let span = &mut line.spans[0];
    span.content = span.content[skip..].to_owned().into();
}

/// Take what comes from byte `at` of `line`'s text on off it.
fn keep_before(line: &mut TextLine<'static>, at: usize) {
    let mut seen = 0;
    let kept = line.spans.iter().position(|span| {
        seen += span.content.len();
        seen >= at
    });
    if let Some(last) = kept {
        line.spans.truncate(last + 1);
        let keep = at - (seen - line.spans[last].content.len());
        let span = &mut line.spans[last];
        span.content = span.content[..keep].to_owned().into();
    }
}

/// `text`'s cells as ratatui draws it, and its widest grapheme's.
fn cells(text: &str) -> (usize, u16) {
    Span::raw(text)
        .styled_graphemes(Style::new())
        .fold((0, 0), |(sum, widest), g| {
            let cells = g.symbol.cell_width();
            (sum + usize::from(cells), widest.max(cells))
        })
}

/// Whether `query` (`lowered` when it is matched ignoring case,
/// [`search::matches`]) can overlap itself: some start of it is also an
/// end of it. Then its matches, taken left to right without overlapping,
/// depend on where the taking starts.
fn overlaps(query: &str, lowered: &str) -> bool {
    let matched = if query.chars().any(char::is_uppercase) {
        query
    } else {
        lowered
    };
    matched
        .char_indices()
        .skip(1)
        .any(|(at, _)| matched.ends_with(&matched[..at]))
}

#[cfg(test)]
mod tests;
