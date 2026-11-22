//! A screen that rewraps its lines when its width changes, for the live
//! area's resize tests: tmux 3.2a does (measured); VTE (GNOME Terminal),
//! kitty, Alacritty, `WezTerm`, Konsole, iTerm2 and Windows Terminal are
//! said to (UNVERIFIED). vt100, which
//! parses the bytes here, keeps each row as drawn on a resize (as xterm
//! does), so the rewrap is this file's: the rows a line wrapped onto are
//! joined back into the line (vt100's `row_wrapped`), and the lines are
//! written again at the new width, the cursor kept on the cell it was
//! on. The screen keeps its top row where it was (VTE's way; tmux keeps
//! the cursor's row and moves what doesn't fit into its history, the
//! same relative layout while nothing reaches the top). Checked against
//! tmux 3.2a by hand: a line ended by a line end stays one row when the
//! screen widens, and one that no longer fits wraps when it narrows.
//! Why not a terminal crate that rewraps on its own (`alacritty_terminal`,
//! wezterm-term): neither is a dependency, and vt100 already is (tui's
//! PTY tests, and indicatif's in-memory screen under the runner's).

use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The screen's height: tall enough that no test's lines scroll.
const ROWS: u16 = 80;

/// The screen, shared with the runner drawing on it.
#[derive(Clone)]
pub(crate) struct Screen(Arc<Mutex<vt100::Parser>>);

/// vt100's parser has no `Debug`; indicatif's `TermLike` needs one.
impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen").finish_non_exhaustive()
    }
}

impl Screen {
    pub(crate) fn new(width: u16) -> Self {
        Screen(Arc::new(Mutex::new(vt100::Parser::new(ROWS, width, 0))))
    }

    fn parser(&self) -> MutexGuard<'_, vt100::Parser> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Bytes as a terminal gets them through a TTY, `\n` sent as `\r\n`
    /// (`ONLCR`).
    pub(crate) fn write(&self, text: &str) {
        self.parser().process(text.replace('\n', "\r\n").as_bytes());
    }

    pub(crate) fn width(&self) -> u16 {
        self.parser().screen().size().1
    }

    /// The rows down to the last one written, their trailing blanks cut.
    pub(crate) fn rows(&self) -> Vec<String> {
        let parser = self.parser();
        let screen = parser.screen();
        let width = screen.size().1;
        let mut rows: Vec<String> = screen
            .rows(0, width)
            .map(|row| row.trim_end().to_string())
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows
    }

    /// Change the width, rewrapping every line as a reflowing terminal
    /// does (the module doc).
    pub(crate) fn resize(&self, width: u16) {
        let mut parser = self.parser();
        let screen = parser.screen();
        let cols = screen.size().1;
        if width == cols {
            return;
        }
        // A cursor past the last column (a row written to its end, the
        // wrap not yet made) is on the last column, as a terminal keeps it.
        let (cursor_row, cursor_col) = screen.cursor_position();
        let cursor_col = cursor_col.min(cols - 1);
        // Each line as its cells' text (a wide character's second cell
        // is empty), and the cursor as a line and a cell in it.
        let mut lines: Vec<Vec<String>> = Vec::new();
        let mut line: Vec<String> = Vec::new();
        let mut cursor = (0, 0);
        for row in 0..ROWS {
            let wrapped = screen.row_wrapped(row);
            let end = if wrapped {
                cols
            } else {
                (0..cols)
                    .rev()
                    .find(|&col| screen.cell(row, col).is_some_and(vt100::Cell::has_contents))
                    .map_or(0, |col| col + 1)
            };
            if row == cursor_row {
                cursor = (lines.len(), line.len() + usize::from(cursor_col));
            }
            for col in 0..end {
                let cell = screen.cell(row, col);
                let text = cell.map_or("", vt100::Cell::contents);
                line.push(
                    if text.is_empty() && !cell.is_some_and(vt100::Cell::is_wide_continuation) {
                        " ".to_string()
                    } else {
                        text.to_string()
                    },
                );
            }
            if !wrapped {
                lines.push(std::mem::take(&mut line));
            }
        }
        lines.truncate(cursor.0 + 1);
        let mut fresh = vt100::Parser::new(ROWS, width, 0);
        let mut cursor_at = (0, 0);
        for (i, cells) in lines.iter().enumerate() {
            if i > 0 {
                fresh.process(b"\r\n");
            }
            let start = fresh.screen().cursor_position().0;
            if i == cursor.0 {
                let offset = u16::try_from(cursor.1).unwrap_or(u16::MAX);
                cursor_at = (start + offset / width, offset % width);
            }
            fresh.process(cells.concat().as_bytes());
        }
        fresh.process(format!("\x1b[{};{}H", cursor_at.0 + 1, cursor_at.1 + 1).as_bytes());
        *parser = fresh;
    }
}

/// What console's `Term` writes for each call
/// (<https://docs.rs/console/0.16.6/console/struct.Term.html>), so the
/// screen gets the bytes stderr would.
impl indicatif::TermLike for Screen {
    fn width(&self) -> u16 {
        Screen::width(self)
    }

    fn height(&self) -> u16 {
        ROWS
    }

    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        if n > 0 {
            self.write(&format!("\x1b[{n}A"));
        }
        Ok(())
    }

    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        if n > 0 {
            self.write(&format!("\x1b[{n}B"));
        }
        Ok(())
    }

    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        if n > 0 {
            self.write(&format!("\x1b[{n}C"));
        }
        Ok(())
    }

    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        if n > 0 {
            self.write(&format!("\x1b[{n}D"));
        }
        Ok(())
    }

    fn write_line(&self, s: &str) -> io::Result<()> {
        self.write(&format!("{s}\n"));
        Ok(())
    }

    fn write_str(&self, s: &str) -> io::Result<()> {
        self.write(s);
        Ok(())
    }

    fn clear_line(&self) -> io::Result<()> {
        self.write("\r\x1b[2K");
        Ok(())
    }

    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line written past the width wraps; widened, it is one row again;
    /// lines ended by line ends stay apart; the cursor stays on its cell.
    #[test]
    fn a_resize_rewraps_wrapped_lines_and_keeps_line_ends() {
        let screen = Screen::new(10);
        screen.write("0123456789abcde\nshort\nend");
        assert_eq!(screen.rows(), ["0123456789", "abcde", "short", "end"]);
        screen.resize(20);
        assert_eq!(screen.rows(), ["0123456789abcde", "short", "end"]);
        screen.write("!");
        assert_eq!(screen.rows(), ["0123456789abcde", "short", "end!"]);
        screen.resize(4);
        assert_eq!(
            screen.rows(),
            ["0123", "4567", "89ab", "cde", "shor", "t", "end!"]
        );
    }
}
