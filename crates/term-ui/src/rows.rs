//! The live area's terminal: indicatif's drawing, each row ended by a
//! line end.
//!
//! indicatif pads every line it draws to the terminal's width and starts
//! the next one by the terminal's own wrap, never a line end (indicatif
//! 0.18.6 `DrawState::draw_to_term`: "clear the line and keep the cursor
//! on the right terminal side so that future writes/prints will happen on
//! the next line",
//! <https://docs.rs/crate/indicatif/0.18.6/source/src/draw_target.rs>).
//! To a terminal that rewraps its lines on a resize the live area, and
//! every line printed above it, is then one line wrapped at the old
//! width: widened, it joins rows, so the area's redraw moves up over
//! lines that aren't its own and overwrites them; narrowed, it takes more
//! rows than indicatif counted, so the redraw leaves the area's top
//! behind (the title and steps shown twice). Measured with tmux 3.2a and
//! sb's runner; the maintainer saw it in `sb bench`. Which terminals
//! rewrap is UNVERIFIED but for tmux 3.2a: VTE (GNOME Terminal), kitty,
//! Alacritty, `WezTerm`, Konsole, iTerm2 and Windows Terminal are said to;
//! xterm, `PuTTY` and the Linux console to cut the rows instead.
//!
//! [`Rows`] wraps the terminal indicatif draws on, through indicatif's
//! own extension point
//! (<https://docs.rs/indicatif/0.18.6/indicatif/struct.ProgressDrawTarget.html#method.term_like>),
//! and writes the padding that ends a row as an erase to the row's end
//! and a line end before the next line: the rows Bubble Tea writes, sb-go's
//! live views (`\r\n` and `EraseLineRight` after each line,
//! charmbracelet/bubbletea v2.1.0 `cursed_renderer.go`, `insertAbove`).
//! A line as wide as the screen gets no erase: the cursor waits on its
//! last column for the wrap, and an erase there takes that column's
//! character on xterm and VTE (Bubble Tea v1.3.4 `standard_renderer.go`,
//! `flush`, erases only a line narrower than the screen for this reason).
//! What indicatif draws, and where, stays its own.
//!
//! Narrowed past a line's width, a reflowing terminal still wraps that
//! line onto more rows, which indicatif doesn't count: its redraw moves
//! up one row a line (`bar_count`, its count at the width it drew at). So
//! the first move up of a redraw after the screen narrowed also covers
//! the rows the last draw's lines now take at the new width, and clears
//! each of them before the redraw, as indicatif's own clear does (only
//! then: a clear before the rows are drawn again is what the live area
//! otherwise never does, as it blinks). Why not one erase to the
//! screen's end: from the screen's top-left corner tmux takes it as a
//! clear and keeps the old screen in its history (measured, tmux 3.2a),
//! and after the last row it could take that row's last character, as
//! above. The count is UNVERIFIED but for tmux 3.2a, and
//! leaves out a wide character pushed to the next row at the wrap
//! (indicatif's own count, `wrapped_metrics`, is private), so after such
//! a narrowing a row can stay behind. A terminal that doesn't rewrap
//! (xterm, `PuTTY`, the Linux console, by their documentation) cuts those
//! lines instead, so there the redraw starts that many rows higher, over
//! the lines above the area: which a terminal does can't be asked without
//! reading its replies from the input, and most rewrap (the maintainer's
//! does). No dependency counts rows for a resize either: Bubble Tea's
//! inline renderer redraws from the rows it last wrote (v2.1.0
//! `cursed_renderer.go`, `resize`). Why not another
//! progress crate or ratatui's inline viewport: the runner's live area is
//! indicatif's (its module doc), and the viewport is clamped to the
//! screen's height, which the tree's own collapse already handles
//! (`Tree::live`).

use std::io;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use crossterm::terminal::{Clear, ClearType};
use indicatif::TermLike;

/// `command`'s escape code, as crossterm writes it
/// (<https://docs.rs/crossterm/0.29.0/crossterm/trait.Command.html#tymethod.write_ansi>).
fn ansi(command: impl crossterm::Command) -> String {
    let mut code = String::new();
    // Writing to a `String` doesn't fail.
    let _ = command.write_ansi(&mut code);
    code
}

/// Erase from the cursor to the row's end, made once: it ends every row
/// of every frame.
static ROW_END: LazyLock<String> = LazyLock::new(|| ansi(Clear(ClearType::UntilNewLine)));

/// `term`, its rows ended by line ends (the module doc).
#[derive(Debug)]
pub(crate) struct Rows<T> {
    term: T,
    state: Mutex<Row>,
}

/// Where the cursor is on the row being written.
#[derive(Debug, Default)]
struct Row {
    /// The width indicatif last read, which it pads each line to.
    width: usize,
    /// The columns written on the row so far.
    column: usize,
    /// The row's padding was left out: the next text starts on a row of
    /// its own.
    ended: bool,
    /// A draw is under way: written to since the last flush.
    drawing: bool,
    /// The widths of the lines of the draw under way.
    lines: Vec<usize>,
    /// The last draw's lines' widths.
    drawn: Vec<usize>,
}

impl Row {
    /// The rows the last `lines` lines of the last draw take at `width`
    /// beyond one each: none while it is as wide as they were drawn at
    /// or wider (a line is never wider than the width it was drawn at).
    fn rewrapped(&self, lines: usize, width: usize) -> usize {
        let last = &self.drawn[self.drawn.len().saturating_sub(lines)..];
        last.iter()
            .map(|&w| w.max(1).div_ceil(width.max(1)) - 1)
            .sum()
    }
}

impl<T: TermLike> Rows<T> {
    pub(crate) fn new(term: T) -> Self {
        Rows {
            term,
            state: Mutex::new(Row::default()),
        }
    }

    fn row(&self) -> MutexGuard<'_, Row> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The cursor moved: it is no longer at the end of a row written here.
    fn moved(&self) {
        let mut row = self.row();
        row.column = 0;
        row.ended = false;
    }
}

impl<T: TermLike> TermLike for Rows<T> {
    fn width(&self) -> u16 {
        let width = self.term.width();
        self.row().width = usize::from(width);
        width
    }

    fn height(&self) -> u16 {
        self.term.height()
    }

    /// The first move of a draw goes up over the area's rows: indicatif's
    /// count, `n + 1` lines, and the rows they rewrapped onto since a
    /// narrowing, which are then cleared before it is drawn again (the
    /// module doc).
    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.moved();
        let mut row = self.row();
        let first = !std::mem::replace(&mut row.drawing, true);
        let extra = if first {
            row.rewrapped(n + 1, usize::from(self.term.width()))
        } else {
            0
        };
        drop(row);
        let up = n + extra;
        self.term.move_cursor_up(up)?;
        if extra > 0 {
            // Each row cleared and the cursor back at the top, as
            // indicatif's own clear does (`draw_to_term` without
            // `move_cursor`).
            for i in 0..=up {
                self.term.clear_line()?;
                if i < up {
                    self.term.move_cursor_down(1)?;
                }
            }
            self.term.move_cursor_up(up)?;
        }
        Ok(())
    }

    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.moved();
        self.term.move_cursor_down(n)
    }

    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.moved();
        self.term.move_cursor_right(n)
    }

    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.moved();
        self.term.move_cursor_left(n)
    }

    fn write_line(&self, s: &str) -> io::Result<()> {
        self.moved();
        self.term.write_line(s)
    }

    /// A row's padding (spaces that take it to the width, none for a
    /// line as wide as the screen) becomes an erase to the row's end, or
    /// nothing for a line as wide as the screen (the module doc), and the
    /// next text starts with a line end. Anything else is written as it
    /// is: a line wider than the width indicatif read (the screen
    /// narrowed between the runner's layout and this draw) keeps
    /// indicatif's padding for that one frame, as before.
    fn write_str(&self, s: &str) -> io::Result<()> {
        let mut row = self.row();
        let padding =
            s.bytes().all(|b| b == b' ') && row.width > 0 && row.column + s.len() == row.width;
        row.drawing = true;
        if padding {
            let width = row.column;
            row.lines.push(width);
            row.column = 0;
            row.ended = true;
            drop(row);
            if s.is_empty() {
                return Ok(());
            }
            return self.term.write_str(&ROW_END);
        }
        if std::mem::take(&mut row.ended) {
            self.term.write_str("\r\n")?;
        }
        row.column = match s.rfind('\r') {
            Some(at) => console::measure_text_width(&s[at + 1..]),
            None => row.column + console::measure_text_width(s),
        };
        drop(row);
        self.term.write_str(s)
    }

    fn clear_line(&self) -> io::Result<()> {
        self.moved();
        self.term.clear_line()
    }

    /// A draw's end: its lines' widths kept for the next.
    fn flush(&self) -> io::Result<()> {
        let mut row = self.row();
        row.drawn = std::mem::take(&mut row.lines);
        row.drawing = false;
        drop(row);
        self.term.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reflow::Screen;

    /// indicatif's line and its padding: the line, an erase, and a line
    /// end before the next; the last row keeps the cursor after its text.
    #[test]
    fn padding_becomes_an_erase_and_a_line_end() {
        let screen = Screen::new(10);
        let rows = Rows::new(screen.clone());
        assert_eq!(rows.width(), 10);
        for line in ["one", "", "ten chars!"] {
            rows.write_str(line).expect("line");
            rows.write_str(&" ".repeat(10 - line.len()))
                .expect("padding");
        }
        rows.write_str("x").expect("more");
        screen.resize(20);
        assert_eq!(screen.rows(), ["one", "", "ten chars!", "x"]);
    }

    /// The bytes written to it, on a screen 10 columns wide.
    #[derive(Debug, Default)]
    struct Bytes(Mutex<String>);

    impl Bytes {
        fn take(&self) -> String {
            std::mem::take(&mut self.0.lock().expect("bytes"))
        }

        fn push(&self, s: &str) {
            self.0.lock().expect("bytes").push_str(s);
        }
    }

    impl TermLike for Bytes {
        fn width(&self) -> u16 {
            10
        }
        fn move_cursor_up(&self, n: usize) -> io::Result<()> {
            self.push(&format!("<up {n}>"));
            Ok(())
        }
        fn move_cursor_down(&self, n: usize) -> io::Result<()> {
            self.push(&format!("<down {n}>"));
            Ok(())
        }
        fn move_cursor_right(&self, n: usize) -> io::Result<()> {
            self.push(&format!("<right {n}>"));
            Ok(())
        }
        fn move_cursor_left(&self, n: usize) -> io::Result<()> {
            self.push(&format!("<left {n}>"));
            Ok(())
        }
        fn write_line(&self, s: &str) -> io::Result<()> {
            self.push(&format!("{s}\n"));
            Ok(())
        }
        fn write_str(&self, s: &str) -> io::Result<()> {
            self.push(s);
            Ok(())
        }
        fn clear_line(&self) -> io::Result<()> {
            self.push("<clear>");
            Ok(())
        }
        fn flush(&self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A line as wide as the screen gets no erase: the cursor stays on
    /// its last column, the wrap pending, and an erase there would take
    /// that column's character with it (xterm's and VTE's way; tmux and
    /// vt100 keep it, so the screen can't show it). The line end that
    /// starts the next line is all it needs.
    #[test]
    fn a_full_width_line_is_not_erased() {
        let rows = Rows::new(Bytes::default());
        rows.width();
        rows.write_str("ten chars!").expect("line");
        rows.write_str("").expect("no padding");
        rows.write_str("next").expect("next");
        rows.write_str(&" ".repeat(6)).expect("padding");
        assert_eq!(rows.term.take(), "ten chars!\r\nnext\x1b[K");
    }

    /// indicatif's draw of `lines` on `rows`, as `draw_to_term` makes it:
    /// up over the last draw's `up + 1` lines, each line and its padding.
    fn draw(rows: &Rows<Screen>, up: usize, lines: &[&str]) {
        rows.move_cursor_up(up).expect("up");
        rows.write_str("\r").expect("cr");
        let width = usize::from(rows.width());
        for line in lines {
            rows.write_str(line).expect("line");
            rows.write_str(&" ".repeat(width - line.len()))
                .expect("padding");
        }
        rows.flush().expect("flush");
    }

    /// Narrowed, the next draw moves up over the rows the last one's
    /// lines were rewrapped onto too, and clears them before it draws;
    /// widened, it moves up one row a line, as indicatif counts.
    #[test]
    fn a_draw_after_a_narrowing_covers_the_rewrapped_rows() {
        let screen = Screen::new(40);
        screen.write("above\n");
        let rows = Rows::new(screen.clone());
        draw(
            &rows,
            0,
            &["a line thirty columns wide....", "two", "three"],
        );
        screen.resize(12);
        draw(&rows, 2, &["one", "two"]);
        assert_eq!(screen.rows(), ["above", "one", "two"]);
        screen.resize(30);
        draw(&rows, 1, &["one", "two", "three"]);
        assert_eq!(screen.rows(), ["above", "one", "two", "three"]);
    }

    /// A call on the recorder through `Rows`.
    type Op = fn(&Rows<Bytes>) -> io::Result<()>;

    /// The cursor's other moves, a whole line and a cleared row pass
    /// through as they are, and each ends the row written here: the next
    /// text starts where the cursor is, with no line end before it.
    #[test]
    fn moves_and_whole_lines_pass_through() {
        let rows = Rows::new(Bytes::default());
        assert_eq!(rows.width(), 10);
        assert_eq!(rows.height(), 20);
        let ops: [(&str, Op); 5] = [
            ("<down 1>", |rows| rows.move_cursor_down(1)),
            ("<right 2>", |rows| rows.move_cursor_right(2)),
            ("<left 3>", |rows| rows.move_cursor_left(3)),
            ("whole\n", |rows| rows.write_line("whole")),
            ("<clear>", TermLike::clear_line),
        ];
        for (written, op) in ops {
            rows.write_str("ten chars!").expect("line");
            rows.write_str("").expect("no padding");
            op(&rows).expect("op");
            rows.write_str("x").expect("x");
            rows.flush().expect("flush");
            assert_eq!(rows.term.take(), format!("ten chars!{written}x"));
            rows.moved();
        }
    }

    /// Spaces that don't end the row are text; after a cursor move the
    /// next text starts where the cursor is.
    #[test]
    fn other_spaces_and_moves_are_kept() {
        let screen = Screen::new(10);
        let rows = Rows::new(screen.clone());
        rows.width();
        rows.write_str("a").expect("a");
        rows.write_str("  ").expect("spaces");
        rows.write_str("b").expect("b");
        rows.write_str(&" ".repeat(6)).expect("padding");
        rows.move_cursor_up(0).expect("up");
        rows.write_str("\r").expect("cr");
        rows.write_str("c").expect("c");
        assert_eq!(screen.rows(), ["c  b"]);
    }
}
