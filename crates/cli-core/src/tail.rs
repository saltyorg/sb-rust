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
//! the terminal, which a run ending by a signal can't wait for. Why not a
//! terminal emulator (vt100, a test-only dependency here): it needs
//! every byte the terminal got, and sb never sees an attached child's
//! nor indicatif's live redraws. No tracked writer moves the cursor or
//! erases lines today; one that did would have to be recorded as
//! [`other`].
//!
//! When a run ends by SIGINT, an interactive shell (bash, zsh) prints a
//! newline of its own after the job (for the `^C` echo), so the ending is
//! one newline shorter then ([`finish`]).

use std::io::{self, IsTerminal as _, Write};
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};

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
                // A return or whitespace leaves the line as it looks (the
                // whitespace anstream's stripper keeps).
                b'\r' | b' ' | b'\t' | b'\x0c' => {}
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
    /// blank, one newline after an ended line, two in the middle of one;
    /// one fewer when the run ends by SIGINT (`by_sigint`), after which
    /// the shell prints a newline of its own.
    #[must_use]
    pub fn ending(self, by_sigint: bool) -> &'static str {
        match (self.wrote, self.line, by_sigint) {
            (true, Line::Ended { blank: false }, false) | (true, Line::Open, true) => "\n",
            (true, Line::Open, false) => "\n\n",
            // Nothing visible, a blank last line, or the shell's newline
            // after an ended one.
            _ => "",
        }
    }

    /// What output sb doesn't see leaves: a line that ended, not blank.
    const OTHER: Tail = Tail {
        wrote: true,
        line: Line::Ended { blank: false },
    };
}

/// The run's tail and each stream's stripper.
struct State {
    tail: Tail,
    stdout: StripBytes,
    stderr: StripBytes,
}

fn state() -> MutexGuard<'static, State> {
    // Not a const static: `StripBytes::new` isn't `const`.
    static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
        Mutex::new(State {
            tail: Tail::default(),
            stdout: StripBytes::new(),
            stderr: StripBytes::new(),
        })
    });
    // The tail is whole between calls, so a poisoned lock is usable.
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
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
    state().tail = Tail::OTHER;
}

/// The tail so far.
#[must_use]
pub fn current() -> Tail {
    state().tail
}

/// The run's end: when stderr is a terminal, what [`Tail::ending`] says
/// (`by_sigint`: the run ends by SIGINT), written to `out` (sb's stderr),
/// then recorded. Nothing otherwise, so a pipe or a file gets the same
/// bytes as before. stdout is flushed first, so a line it still holds
/// comes before the ending.
pub fn finish(out: &mut impl Write, by_sigint: bool) {
    if !Stream::Stderr.is_terminal() {
        return;
    }
    let _ = io::stdout().flush();
    let ending = current().ending(by_sigint);
    if ending.is_empty() {
        return;
    }
    // Nothing better to do if stderr fails at the very end.
    let _ = out.write_all(ending.as_bytes()).and_then(|()| out.flush());
    note(Stream::Stderr, ending.as_bytes());
}

/// sb's stdout (anstream's, so `--color` and `NO_COLOR` hold), recorded.
#[must_use]
pub fn stdout() -> Tracked<anstream::Stdout> {
    Tracked::stdout(anstream::stdout())
}

/// sb's stderr (anstream's), recorded.
#[must_use]
pub fn stderr() -> Tracked<anstream::Stderr> {
    Tracked::stderr(anstream::stderr())
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
        let ending = |text: &str| tail(text).ending(false);
        assert_eq!(ending(""), "");
        assert_eq!(ending("sb 1.0.0\n"), "\n");
        assert_eq!(ending("error\n\n"), "");
        assert_eq!(ending("? Continue [y/N] "), "\n\n");
        // Only whitespace, or a return, is still blank.
        assert_eq!(ending("text\n   \n"), "");
        assert_eq!(ending("text\n\r\n"), "");
        assert_eq!(ending("text\n\t\n"), "");
        assert_eq!(ending("text\n\x0c\n"), "");
        // Text after a blank line counts again.
        assert_eq!(ending("\n\nmore\n"), "\n");
        // Spaces alone wrote nothing visible.
        assert_eq!(ending("   "), "");
    }

    /// Ended by SIGINT, the shell's own newline is one of them: one
    /// newline fewer, never a negative.
    #[test]
    fn by_sigint_the_shells_newline_counts() {
        let ending = |text: &str| tail(text).ending(true);
        assert_eq!(ending(""), "");
        assert_eq!(ending("✗ interrupted\n"), "");
        assert_eq!(ending("halfway"), "\n");
        assert_eq!(ending("done\n\n"), "");
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
        assert_eq!(tail.ending(false), "");
    }

    /// What a child wrote is a line that ended, not blank. (The value
    /// [`other`] sets; the process-wide tail isn't touched here.)
    #[test]
    fn output_sb_doesnt_see_is_an_ended_line() {
        assert_eq!(Tail::OTHER.ending(false), "\n");
        assert_eq!(Tail::OTHER.after(b"next\n").ending(false), "\n");
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
