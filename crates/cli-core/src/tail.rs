//! The blank line at the end of a run (cli-core spec, "The blank line at
//! the end"; the maintainer, 2026-11-19): when stderr is a terminal, a run
//! ends with one blank line between sb's last visible line and the shell's
//! next prompt, unless what sb left on the terminal ends in one already.
//!
//! What sb writes to a terminal, on stdout or stderr, is recorded here
//! ([`Tracked`], [`note`]): whether anything visible was written, whether
//! the last line ended, and whether it was blank. Escape sequences are
//! left out with anstream's stripper, which keeps its state across writes
//! (<https://docs.rs/anstream/1.0.0/anstream/adapter/struct.StripBytes.html>).
//! What sb can't see (an attached child's output) is recorded as a line
//! that ended and wasn't blank ([`other`]). [`finish`] writes the line.
//!
//! Why not ask the terminal: the cursor position report (`CSI 6 n`)
//! gives the row and column, not whether the line above is blank, and no
//! terminal sequence reads a line back; and it needs a reply read from
//! the terminal, which a run ending by a signal can't wait for.

use std::io::{self, IsTerminal as _, Write};
use std::sync::{Mutex, OnceLock, PoisonError};

use anstream::adapter::StripBytes;

/// Which of sb's streams a write went to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    /// Whether the stream is a terminal, read once per run.
    fn is_terminal(self) -> bool {
        static STDOUT: OnceLock<bool> = OnceLock::new();
        static STDERR: OnceLock<bool> = OnceLock::new();
        match self {
            Stream::Stdout => *STDOUT.get_or_init(|| io::stdout().is_terminal()),
            Stream::Stderr => *STDERR.get_or_init(|| io::stderr().is_terminal()),
        }
    }
}

/// What the terminal shows at its end, as far as sb wrote it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tail {
    /// Something visible was written.
    wrote: bool,
    /// Where the last line is.
    line: Line,
}

/// The last line's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Line {
    /// Nothing written yet.
    #[default]
    Start,
    /// It ended; `blank`: it held nothing visible.
    Ended { blank: bool },
    /// It has something visible and hasn't ended.
    Open,
}

impl Tail {
    /// `text` (escape sequences already left out) written after what this
    /// tail says.
    #[must_use]
    pub fn after(mut self, text: &[u8]) -> Self {
        for &byte in text {
            match byte {
                b'\n' => {
                    self.line = Line::Ended {
                        blank: self.line != Line::Open,
                    };
                }
                // A return or a space leaves the line as it looks.
                b'\r' | b' ' | b'\t' => {}
                _ => {
                    self.line = Line::Open;
                    self.wrote = true;
                }
            }
        }
        self
    }

    /// What the run's end writes for one blank line before the prompt:
    /// nothing when nothing visible was written or the last line is
    /// blank, one newline after an ended line, two in the middle of one.
    #[must_use]
    pub fn ending(self) -> &'static str {
        match (self.wrote, self.line) {
            (false, _) | (true, Line::Start | Line::Ended { blank: true }) => "",
            (true, Line::Ended { blank: false }) => "\n",
            (true, Line::Open) => "\n\n",
        }
    }
}

/// The run's tail and each stream's stripper.
struct State {
    tail: Tail,
    stdout: StripBytes,
    stderr: StripBytes,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| {
            Mutex::new(State {
                tail: Tail::default(),
                stdout: StripBytes::new(),
                stderr: StripBytes::new(),
            })
        })
        .lock()
        // The tail is whole between calls, so a poisoned lock is usable.
        .unwrap_or_else(PoisonError::into_inner)
}

/// `bytes` written to `stream`, recorded when it is a terminal.
pub fn note(stream: Stream, bytes: &[u8]) {
    if !stream.is_terminal() {
        return;
    }
    let mut state = state();
    let State {
        tail,
        stdout,
        stderr,
    } = &mut *state;
    let stripper = match stream {
        Stream::Stdout => stdout,
        Stream::Stderr => stderr,
    };
    for text in stripper.strip_next(bytes) {
        *tail = tail.after(text);
    }
}

/// Output sb doesn't see went to the terminal (an attached child's): a
/// line that ended and wasn't blank, as far as the end is concerned.
pub fn other() {
    let mut state = state();
    state.tail = Tail {
        wrote: true,
        line: Line::Ended { blank: false },
    };
}

/// The tail so far.
#[must_use]
pub fn current() -> Tail {
    state().tail
}

/// The run's end: when stderr is a terminal, what [`Tail::ending`] says,
/// written to `out` (sb's stderr), then recorded. Nothing otherwise, so a
/// pipe or a file gets the same bytes as before.
pub fn finish(out: &mut impl Write) {
    if !Stream::Stderr.is_terminal() {
        return;
    }
    let ending = current().ending();
    if ending.is_empty() {
        return;
    }
    // Nothing better to do if stderr fails at the very end.
    let _ = out.write_all(ending.as_bytes()).and_then(|()| out.flush());
    note(Stream::Stderr, ending.as_bytes());
}

/// A writer whose bytes are recorded as written to `stream`.
#[derive(Debug)]
pub struct Tracked<W> {
    inner: W,
    stream: Stream,
}

impl<W: Write> Tracked<W> {
    /// `inner`, sb's stdout.
    pub fn stdout(inner: W) -> Self {
        Self {
            inner,
            stream: Stream::Stdout,
        }
    }

    /// `inner`, sb's stderr.
    pub fn stderr(inner: W) -> Self {
        Self {
            inner,
            stream: Stream::Stderr,
        }
    }
}

impl<W: Write> Write for Tracked<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        note(self.stream, &buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail(text: &str) -> Tail {
        Tail::default().after(text.as_bytes())
    }

    /// One blank line before the prompt, never two.
    #[test]
    fn the_ending_makes_one_blank_line() {
        assert_eq!(tail("").ending(), "");
        assert_eq!(tail("sb 1.0.0\n").ending(), "\n");
        assert_eq!(tail("error\n\n").ending(), "");
        assert_eq!(tail("? Continue [y/N] ").ending(), "\n\n");
        // Only spaces, or a return, is still blank.
        assert_eq!(tail("text\n   \n").ending(), "");
        assert_eq!(tail("text\n\r\n").ending(), "");
        assert_eq!(tail("text\n\t\n").ending(), "");
        // Text after a blank line counts again.
        assert_eq!(tail("\n\nmore\n").ending(), "\n");
        // Spaces alone wrote nothing visible.
        assert_eq!(tail("   ").ending(), "");
    }

    /// Escape sequences don't count: a line of colour codes is blank, a
    /// coloured line isn't, and a code split across writes is still left
    /// out (anstream's stripper keeps its state).
    #[test]
    fn escape_sequences_are_left_out() {
        let mut stripper = StripBytes::new();
        let mut tail = Tail::default();
        for chunk in [&b"\x1b[3"[..], b"1mred\x1b[0m\n", b"\x1b[0m\n"] {
            for text in stripper.strip_next(chunk) {
                tail = tail.after(text);
            }
        }
        assert_eq!(
            tail,
            Tail {
                wrote: true,
                line: Line::Ended { blank: true }
            }
        );
        assert_eq!(tail.ending(), "");
    }

    /// What a child wrote is a line that ended, not blank.
    #[test]
    fn output_sb_doesnt_see_is_an_ended_line() {
        other();
        assert_eq!(current().ending(), "\n");
    }

    /// The tracked writer passes every byte through as it is.
    #[test]
    fn a_tracked_writer_passes_its_bytes_through() {
        let mut out = Tracked::stdout(Vec::new());
        out.write_all(b"one\x1b[0m\n").expect("write");
        out.flush().expect("flush");
        assert_eq!(out.inner, b"one\x1b[0m\n");
        let mut err = Tracked::stderr(Vec::new());
        writeln!(err, "two").expect("write");
        assert_eq!(err.inner, b"two\n");
    }
}
