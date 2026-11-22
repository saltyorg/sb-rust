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
//! What indicatif draws, and where, stays its own. Why not another
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

    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.moved();
        self.term.move_cursor_up(n)
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
        if padding {
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

    fn flush(&self) -> io::Result<()> {
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
