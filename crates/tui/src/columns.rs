//! A styled screen line cut at a column: what ratatui's
//! `Paragraph::scroll` x offset shows of it, as a line to draw from its
//! first column, found without walking what lies left of the column on
//! each frame (the viewer with wrap off, the tui spec's success criterion
//! 8 far to the right).
//!
//! Why not `Paragraph::scroll` alone
//! (<https://docs.rs/ratatui/0.30.2/ratatui/widgets/struct.Paragraph.html#method.scroll>):
//! its `LineTruncator`
//! (<https://docs.rs/crate/ratatui-widgets/0.3.2/source/src/reflow.rs>)
//! walks every grapheme left of the offset, styled, on each draw, so a
//! frame of 70 records of 5,000 characters at column 4,000 took 14 ms
//! against the spec's 5 ms. ratatui has no public cut of a `Line` at a
//! column (`Line`'s own `spans_after_width` is private and for centred and
//! right-aligned lines), and its scroll keeps no state between frames.
//!
//! The cut follows `LineTruncator` for a left-aligned line (the viewer's
//! lines set no alignment, and ratatui scrolls no other): graphemes come
//! from `Span::styled_graphemes`, which drops those holding a control
//! character (as `CellWidth`'s docs say,
//! <https://docs.rs/ratatui/0.30.2/ratatui/buffer/trait.CellWidth.html>),
//! each `CellWidth::cell_width` columns wide, one wider than the window
//! taking none. Those that end at or before the column are left out; the
//! first that starts at the column or crosses it is the first shown,
//! whole (a wide character the column splits is shown from its first
//! cell, as ratatui shows it). Each span keeps its style and the line its
//! own, so a style that began left of the column still applies.
//! UNVERIFIED against ratatui's docs, which don't describe the offset
//! this closely: checked by drawing both ways in
//! `a_cut_draws_as_the_scrolled_line` (wide, combining, joined and paired
//! emoji, halfwidth sound marks, control characters, empty spans).
//!
//! [`Marks`] are where a cut can start without a walk: each span's start,
//! and inside a span of other than printable ASCII (where a byte is a
//! column) a grapheme every [`EVERY`] columns or graphemes. They are made
//! once for a text, so a frame's cut walks at most that many graphemes.

use ratatui::buffer::CellWidth as _;
use ratatui::style::Style;
use ratatui::text::{Line as TextLine, Span, Text};

/// The columns (or graphemes, for those of no width) between marks inside
/// a span of other than printable ASCII: a cut walks at most this many,
/// and its line reaches at most this many past the window. Why 64: a
/// quarter of a 240-column window's worth of walking and handing over at
/// most, for a mark of 32 bytes every 64 columns, a twentieth of what
/// 64 columns of CJK text take in the text itself.
const EVERY: usize = 64;

/// A place a cut can start or end: a grapheme's start, by span and byte,
/// and the column it starts at. `ascii`: up to the next mark, each byte is
/// a grapheme one column wide.
#[derive(Clone, Copy, Debug)]
struct Mark {
    column: usize,
    span: usize,
    byte: usize,
    ascii: bool,
}

/// A screen line's [`Mark`]s, and where they stop short of its end, if
/// they do.
#[derive(Debug)]
struct Row {
    marks: Vec<Mark>,
    stop: Option<(usize, usize)>,
}

/// The [`Row`]s of each screen line of a text, for a window `width`
/// columns wide (a grapheme wider than the window takes no column),
/// through column `reach`.
#[derive(Debug)]
pub(crate) struct Marks {
    width: u16,
    lines: Vec<Row>,
}

impl Marks {
    /// The marks of `text`'s lines in a window `width` columns wide,
    /// through column `reach` (`usize::MAX`: all): no window shows more of
    /// a text than it holds right ([`crate::line::within`]), so marks past
    /// that would be walked for nothing.
    pub(crate) fn new(text: &Text<'_>, width: u16, reach: usize) -> Self {
        let lines = text
            .lines
            .iter()
            .map(|line| row_of(line, width, reach))
            .collect();
        Self { width, lines }
    }

    /// The window width the marks are for.
    pub(crate) fn width(&self) -> u16 {
        self.width
    }

    /// The marks' bytes: what keeping them costs, less the allocator's own.
    pub(crate) fn size(&self) -> usize {
        self.lines
            .iter()
            .map(|row| size_of::<Row>() + row.marks.capacity() * size_of::<Mark>())
            .sum()
    }

    /// Screen line `at` of `text`, the text the marks were made for, cut
    /// to what a window of their width scrolled to `column` shows, the
    /// window's right edge within their reach; spans borrowed. Past the
    /// end of the line it is empty.
    pub(crate) fn cut<'a>(&self, text: &'a Text<'_>, at: usize, column: usize) -> TextLine<'a> {
        debug_assert_eq!(text.lines.len(), self.lines.len(), "marks of another text");
        let (Some(line), Some(row)) = (text.lines.get(at), self.lines.get(at)) else {
            return TextLine::default();
        };
        let (first, from) = start(line, &row.marks, column, self.width);
        let (last, to) = end(line, row, column.saturating_add(usize::from(self.width)));
        let spans = line
            .spans
            .iter()
            .enumerate()
            .take(last.saturating_add(1))
            .skip(first)
            .filter_map(|(i, span)| {
                let content: &str = &span.content;
                let a = if i == first { from } else { 0 };
                let b = if i == last { to } else { content.len() };
                (a < b).then(|| Span::styled(&content[a..b], span.style))
            })
            .collect();
        TextLine {
            style: line.style,
            alignment: line.alignment,
            spans,
        }
    }
}

/// `line` with its spans borrowed, uncut: what a window
/// at the first column needs, `Paragraph` stopping at its edge.
pub(crate) fn whole<'a>(line: &'a TextLine<'_>) -> TextLine<'a> {
    TextLine {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .iter()
            .map(|span| Span::styled(span.content.as_ref(), span.style))
            .collect(),
    }
}

/// Where the cut of `line` at `column`, in a window `width` wide, starts:
/// the first grapheme shown, by span and byte.
fn start(line: &TextLine<'_>, marks: &[Mark], column: usize, width: u16) -> (usize, usize) {
    // The last mark left of the column: every grapheme before it ends
    // at or before its column, so none of them is shown.
    let Some(mark) = marks
        .partition_point(|m| m.column < column)
        .checked_sub(1)
        .map(|at| marks[at])
    else {
        return (0, 0);
    };
    let content: &str = &line.spans[mark.span].content;
    if mark.ascii {
        return (
            mark.span,
            (mark.byte + column - mark.column).min(content.len()),
        );
    }
    let mut at = mark.column;
    for grapheme in Span::raw(&content[mark.byte..]).styled_graphemes(Style::new()) {
        let cells = cells(grapheme.symbol, width);
        if at >= column || at + cells > column {
            return (mark.span, offset(content, grapheme.symbol));
        }
        at += cells;
    }
    // The span ends at or before the column: the next mark, in a span
    // further on, is at the column or past it.
    (mark.span, content.len())
}

/// Where the cut of `line` that ends at `column` ends: the first grapheme
/// that starts there or after it (or within [`EVERY`] columns after it),
/// by span and byte, past the last span for the line's end.
fn end(line: &TextLine<'_>, row: &Row, column: usize) -> (usize, usize) {
    let marks = &row.marks;
    let at = marks.partition_point(|m| m.column < column);
    match at.checked_sub(1).map(|before| marks[before]) {
        Some(mark) if mark.ascii => {
            let len = line.spans[mark.span].content.len();
            (mark.span, (mark.byte + column - mark.column).min(len))
        }
        _ => marks
            .get(at)
            .map(|mark| (mark.span, mark.byte))
            .or(row.stop)
            .unwrap_or((line.spans.len(), 0)),
    }
}

/// `line`'s marks in a window `width` columns wide, through column
/// `reach`: the walk stops at the first span or grapheme there or past
/// it, and the cut's end with it.
fn row_of(line: &TextLine<'_>, width: u16, reach: usize) -> Row {
    let mut marks = Vec::new();
    let mut column = 0;
    let stop = 'walk: {
        for (span, content) in line.spans.iter().enumerate() {
            if column >= reach {
                break 'walk Some((span, 0));
            }
            let text: &str = &content.content;
            if text.bytes().all(|b| matches!(b, b' '..=b'~')) {
                marks.push(Mark {
                    column,
                    span,
                    byte: 0,
                    ascii: true,
                });
                column += text.len();
                continue;
            }
            // The next mark's column, and the graphemes since the last.
            let mut next = column;
            let mut since = 0;
            for grapheme in content.styled_graphemes(Style::new()) {
                let byte = offset(text, grapheme.symbol);
                if column >= reach {
                    break 'walk Some((span, byte));
                }
                if column >= next || since >= EVERY {
                    marks.push(Mark {
                        column,
                        span,
                        byte,
                        ascii: false,
                    });
                    next = column + EVERY;
                    since = 0;
                }
                since += 1;
                column += cells(grapheme.symbol, width);
            }
        }
        None
    };
    marks.shrink_to_fit();
    Row { marks, stop }
}

/// The columns `grapheme` takes in a window `width` wide: ratatui's
/// width, none when wider than the window (`LineTruncator` skips it).
fn cells(grapheme: &str, width: u16) -> usize {
    let cells = grapheme.cell_width();
    if cells > width { 0 } else { usize::from(cells) }
}

/// The byte offset of `part`, a slice of `whole`, within it (std's
/// `str::substr_range`,
/// <https://doc.rust-lang.org/std/primitive.str.html#method.substr_range>).
pub(crate) fn offset(whole: &str, part: &str) -> usize {
    whole
        .substr_range(part)
        .expect("a grapheme of the text it came from")
        .start
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};
    use ratatui::widgets::{Paragraph, Widget as _};

    use super::*;

    /// `text` drawn `width` wide and `rows` high, scrolled to `column`.
    fn scrolled(text: &Text<'_>, rows: u16, width: u16, column: u16) -> Buffer {
        let area = Rect::new(0, 0, width, rows);
        let mut buffer = Buffer::empty(area);
        Paragraph::new(text.clone())
            .scroll((0, column))
            .render(area, &mut buffer);
        buffer
    }

    /// `text`'s lines cut at `column` for `width`, with marks through
    /// `reach`, drawn from their first column.
    fn cut(text: &Text<'_>, rows: u16, width: u16, column: u16, reach: usize) -> Buffer {
        let marks = Marks::new(text, width, reach);
        let lines: Vec<_> = (0..usize::from(rows))
            .map(|row| marks.cut(text, row, usize::from(column)))
            .collect();
        let cut = Text {
            lines,
            style: text.style,
            alignment: text.alignment,
        };
        let area = Rect::new(0, 0, width, rows);
        let mut buffer = Buffer::empty(area);
        Paragraph::new(cut).render(area, &mut buffer);
        buffer
    }

    fn red() -> Style {
        Style::new().fg(Color::Red)
    }

    /// Lines with every kind of grapheme the cut must count as ratatui
    /// does, in styled spans.
    fn tricky() -> Text<'static> {
        let long_wide = "日本語のログ".repeat(30);
        let long_mixed = "é👩‍💻a🇩🇰🇸🇪ｶﾞ\u{200d}b\u{7}c".repeat(20);
        Text::from(vec![
            TextLine::from(vec![
                Span::raw("plain ascii ".repeat(30)),
                Span::styled("red", red()),
                Span::raw(""),
                Span::styled(long_wide.clone(), red().add_modifier(Modifier::BOLD)),
                Span::raw(" tail"),
            ]),
            TextLine::from(vec![
                Span::styled(long_mixed, red()),
                Span::raw("x"),
                Span::raw("\u{7}"),
                Span::raw("y\tz"),
            ])
            .style(Style::new().bg(Color::Blue)),
            TextLine::from(vec![
                Span::raw(long_wide),
                Span::raw("e\u{301}\u{301}"),
                Span::styled("\u{200b}".repeat(150), red()),
                Span::raw("end"),
            ]),
            TextLine::default(),
            TextLine::from("x".repeat(500)),
        ])
        .style(Style::new().add_modifier(Modifier::ITALIC))
    }

    /// The cut line drawn from its first column is the line scrolled to
    /// the column: every cell and style, at every column through each
    /// line's end and past it, in windows from one column wide, with
    /// marks through the whole line and through the window's edge alone.
    #[test]
    fn a_cut_draws_as_the_scrolled_line() {
        let text = tricky();
        let longest = text.lines.iter().map(TextLine::width).max().unwrap_or(0);
        let last = u16::try_from(longest + 3).expect("short lines");
        for width in [1, 2, 3, 7, 40, 240] {
            for column in 0..=last {
                let scrolled = scrolled(&text, 5, width, column);
                let edge = usize::from(column) + usize::from(width);
                for reach in [usize::MAX, edge] {
                    assert_eq!(
                        cut(&text, 5, width, column, reach),
                        scrolled,
                        "{width} wide at {column}, marks through {reach}"
                    );
                }
            }
        }
    }

    /// A cut reaches about the window: up to the window's end in printable
    /// ASCII, up to a mark's spacing past it elsewhere, wherever the
    /// column is.
    #[test]
    fn a_cut_reaches_about_the_window() {
        let text = tricky();
        let marks = Marks::new(&text, 40, usize::MAX);
        for column in [0, 1, 100, 300, 360, 400, 499] {
            let ascii = marks.cut(&text, 4, column).width();
            assert_eq!(ascii, 40.min(500 - column), "at {column}");
            for row in 0..3 {
                let width = marks.cut(&text, row, column).width();
                assert!(width <= 40 + EVERY + 2, "row {row} at {column}: {width}");
            }
        }
        assert_eq!(marks.cut(&text, 9, 0), TextLine::default());
        // A printable ASCII span that starts past the first column ends
        // at the window's edge too.
        let later = Text::from(TextLine::from(vec![
            Span::raw("ab"),
            Span::styled("x".repeat(500), red()),
        ]));
        let marks = Marks::new(&later, 40, usize::MAX);
        for column in [1, 100, 300] {
            assert_eq!(marks.cut(&later, 0, column).width(), 40, "at {column}");
        }
    }

    /// Marks are each span's start and, outside printable ASCII, one every
    /// [`EVERY`] columns, and their size counts them.
    #[test]
    fn marks_are_span_starts_and_every_so_many_columns() {
        let text = Text::from(TextLine::from(vec![
            Span::raw("ab"),
            Span::raw("日".repeat(100)),
            Span::raw("cd"),
        ]));
        let marks = Marks::new(&text, 80, usize::MAX);
        assert_eq!(marks.width(), 80);
        assert_eq!(marks.lines[0].stop, None);
        let starts: Vec<_> = marks.lines[0]
            .marks
            .iter()
            .map(|m| (m.column, m.span, m.byte, m.ascii))
            .collect();
        assert_eq!(
            starts,
            [
                (0, 0, 0, true),
                (2, 1, 0, false),
                (66, 1, 96, false),
                (130, 1, 192, false),
                (194, 1, 288, false),
                (202, 2, 0, true),
            ]
        );
        let kept = marks.lines[0].marks.capacity();
        assert!(kept >= 6);
        assert_eq!(marks.size(), size_of::<Row>() + kept * size_of::<Mark>());
        // Through column 100: the walk stops at the first grapheme there,
        // and through column 2 at the second span.
        let short = Marks::new(&text, 80, 100);
        assert_eq!(short.lines[0].marks.len(), 3);
        assert_eq!(short.lines[0].stop, Some((1, 147)));
        let shorter = Marks::new(&text, 80, 2);
        assert_eq!(shorter.lines[0].marks.len(), 1);
        assert_eq!(shorter.lines[0].stop, Some((1, 0)));
        assert_eq!(shorter.cut(&text, 0, 0).spans, [Span::raw("ab")]);
        // Graphemes of no width (U+200B, three bytes each) get a mark every
        // so many graphemes, so a cut never walks a long run of them.
        let run = Text::from(TextLine::from("\u{200b}".repeat(200)));
        let marks = Marks::new(&run, 80, usize::MAX);
        let starts: Vec<_> = marks.lines[0]
            .marks
            .iter()
            .map(|m| (m.column, m.byte))
            .collect();
        assert_eq!(starts, [(0, 0), (0, 192), (0, 384), (0, 576)]);
    }
}
