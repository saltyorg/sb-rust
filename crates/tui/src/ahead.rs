//! Resume places, and far-right texts, made ahead of time on a thread of
//! their own (the follow-up "First sight far right meets 5 ms"): with
//! wrap off, the viewer asks for the lines it may show next (the screens
//! around the window, the buffer's two ends, the lines around the
//! search's next and previous match). For each the worker finds the
//! line's resume places through its whole message
//! ([`crate::resume::scan`]) and, away from the first column, builds the
//! text the window there shows ([`crate::resume::far`]), with the format,
//! the search and the window the viewer gave. A line drawn far right for
//! the first time then takes that text as it is, or, after the window
//! moved, builds it from the places near the window, instead of parsing
//! the line from its start.
//!
//! - One worker thread, started the first time anything is asked for,
//!   through `sb_cli_core::spawn::thread` (sb's one way to start a
//!   thread, which carries the run's span). It ends when the viewer
//!   drops its sender: std's `mpsc` iterator ends once every sender is
//!   gone (<https://doc.rust-lang.org/std/sync/mpsc/struct.Receiver.html#method.iter>).
//! - Each finished line goes back on a second channel, and the worker
//!   calls `notify_one` on the screen's wake, as a source does after an
//!   event, so the viewer takes it in at its next wake
//!   (<https://docs.rs/tokio/1.54.1/tokio/sync/struct.Notify.html#method.notify_one>).
//! - The lines wanted change as the window moves; the worker skips a
//!   line no longer wanted (a set it shares with the viewer), so it never
//!   works behind the window for long. A text made ahead serves only
//!   the format and search it was made with, and only while it holds the
//!   window (a few steps sideways, as a kept text does): each carries the
//!   [`Window`] it was made for.
//! - Bounded: at most [`AHEAD_BYTES`] of places and texts are kept, and
//!   only for lines still wanted; a line whose places or text don't fit
//!   isn't asked for again while it stays wanted.
//!
//! - A panic on the worker (its parse of untrusted bytes) is not
//!   swallowed: the viewer joins the thread when its channels close and
//!   resumes the panic (`JoinHandle::join`, `std::panic::resume_unwind`,
//!   as cli-core's YAML thread and the journal source do), so the screen
//!   ends as it would had the parse panicked on its own thread.
//!
//! Why not tokio's blocking pool (`sb_cli_core::spawn::blocking`): tokio
//! advises a thread of its own for a worker that runs as long as its
//! caller (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>,
//! "When to use `spawn_blocking` vs dedicated threads"), and it keeps this
//! work, which can take tens of milliseconds a screen, apart from sb's
//! other blocking work.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{
    Receiver, Sender, SyncSender, TryRecvError, TrySendError, channel, sync_channel,
};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use ratatui::text::{Line as TextLine, Span, Text};
use tokio::sync::Notify;

use crate::line::{self, Format};
use crate::resume::{self, Far, Resumes};
use crate::source::Line;

/// The bytes of places and texts kept ahead, at most: places are about
/// 40 bytes every 64 columns and a far-right text a few hundred columns,
/// so a few hundred lines of 5,000 columns take one or two megabytes;
/// this leaves room for longer lines and is an eighth of the buffer's
/// 32 MiB, like the frame cache's bound.
pub(crate) const AHEAD_BYTES: usize = 4 << 20;

/// The lines asked for at once, at most, and the jobs the worker's queue
/// holds: each is copied for the worker on the viewer's thread (a
/// 5,000-character line, five kilobytes or more), so a frame that moves
/// the whole band (Home, End) asks for the nearest and the rest follow
/// as the worker's results wake the viewer.
const ASK_AT_ONCE: usize = 32;

/// What a far-right text is made for: the format and search it is built
/// with, the columns it holds (`left` through `reach`, [`resume::far`]),
/// and the window it is for, from `column`, `width` wide.
#[derive(Debug, PartialEq)]
pub(crate) struct Window {
    pub(crate) format: Format,
    pub(crate) query: String,
    pub(crate) left: usize,
    pub(crate) reach: usize,
    pub(crate) column: usize,
    pub(crate) width: u16,
}

impl Window {
    /// Whether a text made for `self` serves `now` too: the same format
    /// and search, and `now`'s columns within those it holds (as a kept
    /// text serves a sideways step).
    fn covers(&self, now: &Window) -> bool {
        self.format == now.format
            && self.query == now.query
            && self.left <= now.column
            && self.reach >= now.column + usize::from(now.width)
    }
}

/// A line to make ahead: its id in the viewer, the line, the format its
/// message is shown with, its places so far (`None`: none yet), and the
/// window to build its text for (`None`: places only).
#[derive(Debug)]
struct Job {
    id: u64,
    line: Line,
    format: Arc<Format>,
    resumes: Option<Resumes>,
    window: Option<Arc<Window>>,
}

/// What the worker made for a line: its places, and its text with the
/// window it was made for.
#[derive(Debug)]
struct Made {
    resumes: Resumes,
    text: Option<(Arc<Window>, Far)>,
}

impl Made {
    /// What keeping it costs, less the allocator's own.
    fn size(&self) -> usize {
        self.resumes.size()
            + self
                .text
                .as_ref()
                .map_or(0, |(_, far)| text_bytes(&far.text))
    }
}

/// A line the worker finished: its id, the window it was asked for,
/// what was made, and whether its places were found from its start.
#[derive(Debug)]
struct Done {
    id: u64,
    window: Option<Arc<Window>>,
    made: Made,
    #[cfg(test)]
    fresh: bool,
}

/// The viewer's ends of the worker's channels, and the thread.
#[derive(Debug)]
struct Worker {
    jobs: SyncSender<Job>,
    done: Receiver<Done>,
    thread: JoinHandle<()>,
}

/// What is made ahead, the lines asked for and not yet back (with the
/// window they were asked for), and the worker.
#[derive(Debug)]
pub(crate) struct Ahead {
    /// The screen's wake, for the worker; `None`: nothing is made ahead.
    wake: Option<Arc<Notify>>,
    worker: Option<Worker>,
    /// The lines wanted, shared with the worker, which skips the others.
    wanted: Arc<Mutex<HashSet<u64>>>,
    /// The format lines are made with now.
    format: Option<Arc<Format>>,
    /// The window texts are made for now.
    window: Option<Arc<Window>>,
    /// The lines asked for and not yet back, with the window each was
    /// last asked for.
    pending: HashMap<u64, Option<Arc<Window>>>,
    /// What was made, by line.
    store: HashMap<u64, Made>,
    /// The store's bytes ([`Made::size`]), at most `limit`.
    bytes: usize,
    /// Lines whose places or text didn't fit: not asked for again until
    /// the window moves.
    refused: HashSet<u64>,
    limit: usize,
    /// The last ask stopped at [`ASK_AT_ONCE`]: more lines wanted.
    more: bool,
    /// Lines whose places the worker found from their start.
    #[cfg(test)]
    fresh: usize,
}

/// Nothing made ahead, ever: no wake to give the worker.
impl Default for Ahead {
    fn default() -> Self {
        Self {
            wake: None,
            worker: None,
            wanted: Arc::default(),
            format: None,
            window: None,
            pending: HashMap::new(),
            store: HashMap::new(),
            bytes: 0,
            refused: HashSet::new(),
            limit: AHEAD_BYTES,
            more: false,
            #[cfg(test)]
            fresh: 0,
        }
    }
}

impl Ahead {
    /// Nothing asked for yet; the worker wakes `wake` with each line done.
    pub(crate) fn new(wake: Arc<Notify>) -> Self {
        Self {
            wake: Some(wake),
            ..Self::default()
        }
    }

    /// Want the lines `ids`, in the order they may be needed, shown with
    /// `format`: their places, and for those marked, with `window`, their
    /// texts made for it. `line` gives a line by id (`None`: nothing to
    /// make, such as a line that fits in the window). What was made for
    /// lines no longer wanted goes.
    pub(crate) fn want<'a>(
        &mut self,
        ids: &[(u64, bool)],
        format: &Format,
        window: Option<Window>,
        line: impl Fn(u64) -> Option<&'a Line>,
    ) {
        self.drain();
        let wanted: HashSet<u64> = ids.iter().map(|&(id, _)| id).collect();
        // Only what goes is measured: a frame drops a screen or so.
        let mut gone = 0;
        self.store.retain(|id, made| {
            let keep = wanted.contains(id);
            if !keep {
                gone += made.size();
            }
            keep
        });
        self.bytes -= gone;
        self.pending.retain(|id, _| wanted.contains(id));
        self.refused.retain(|id| wanted.contains(id));
        *self.wanted.lock().unwrap_or_else(PoisonError::into_inner) = wanted;
        if self.format.as_deref() != Some(format) {
            // Places of another colour or record cap are another text's
            // (`Resumes`, which `resume::far` resets too); a theme, zone or
            // `t` change keeps them.
            let shown_alike = self.format.as_deref().is_some_and(|old| {
                old.colour() == format.colour() && old.record_cap == format.record_cap
            });
            if !shown_alike {
                self.store.clear();
                self.bytes = 0;
                self.pending.clear();
                self.refused.clear();
            }
            self.format = Some(Arc::new(format.clone()));
        }
        if window.as_ref() != self.window.as_deref() {
            self.window = window.map(Arc::new);
            // Space may have come free since: ask again.
            self.refused.clear();
        }
        self.more = false;
        let Some(jobs) = self.worker().map(|w| w.jobs.clone()) else {
            return;
        };
        let mut asked_now = 0;
        for &(id, text) in ids {
            // The window this line's text is wanted for, if any.
            let now = self.window.clone().filter(|_| text);
            let made = self.store.get(&id);
            let done = made.is_some_and(|m| match &now {
                None => true,
                Some(now) => m.text.as_ref().is_some_and(|(w, _)| w.covers(now)),
            });
            let asked = self
                .pending
                .get(&id)
                .is_some_and(|asked| match (asked, &now) {
                    (_, None) => true,
                    (Some(asked), Some(now)) => asked.covers(now),
                    (None, Some(_)) => false,
                });
            if done || asked || self.refused.contains(&id) {
                continue;
            }
            let Some(line) = line(id) else { continue };
            if asked_now == ASK_AT_ONCE {
                self.more = true;
                return;
            }
            asked_now += 1;
            let job = Job {
                id,
                line: line.clone(),
                format: Arc::clone(self.format.as_ref().expect("set above")),
                resumes: made.map(|m| m.resumes.clone()),
                window: now.clone(),
            };
            match jobs.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // The worker's queue is full: the rest when it has
                    // results.
                    self.more = true;
                    return;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.ended();
                    return;
                }
            }
            self.pending.insert(id, now);
        }
    }

    /// Whether lines are wanted that weren't asked for yet ([`ASK_AT_ONCE`]).
    pub(crate) fn wants_more(&self) -> bool {
        self.more
    }

    /// What was made ahead for line `id`: a copy of its places, which
    /// stay kept (so the line isn't parsed again from its start), and
    /// its text, taken out, if it was made with `format` and `query` (the
    /// caller checks that it holds the window, as for a kept text).
    pub(crate) fn take(
        &mut self,
        id: u64,
        format: &Format,
        query: &str,
    ) -> (Option<Resumes>, Option<Far>) {
        self.drain();
        let Some(made) = self.store.get_mut(&id) else {
            return (None, None);
        };
        let text = made.text.take();
        if let Some((_, far)) = &text {
            self.bytes -= text_bytes(&far.text);
        }
        let text = text
            .filter(|(w, _)| w.format == *format && w.query == query)
            .map(|(_, far)| far);
        (Some(made.resumes.clone()), text)
    }

    /// Take in the lines the worker has finished.
    pub(crate) fn drain(&mut self) {
        loop {
            let Some(worker) = &self.worker else { return };
            match worker.done.try_recv() {
                Ok(done) => self.keep(done),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.ended();
                    return;
                }
            }
        }
    }

    /// The worker's channels closed: it ended, by a panic (resumed here,
    /// on the viewer's thread) or because its thread couldn't go on;
    /// nothing more is made ahead.
    fn ended(&mut self) {
        self.wake = None;
        if let Some(worker) = self.worker.take() {
            drop(worker.jobs);
            if let Err(panic) = worker.thread.join() {
                std::panic::resume_unwind(panic);
            }
        }
    }

    /// Keep `made` for `id`, made for `window`, if it is still wanted and
    /// fits. Made for an earlier ask than the line's last, it is kept
    /// (its text serves while it holds the window) and the line stays
    /// pending.
    fn keep(&mut self, done: Done) {
        let Done {
            id,
            window,
            mut made,
            #[cfg(test)]
            fresh,
        } = done;
        #[cfg(test)]
        {
            self.fresh += usize::from(fresh);
        }
        let window = window.as_ref();
        let Some(asked) = self.pending.get(&id) else {
            return;
        };
        let last = match (asked, window) {
            (None, None) => true,
            (Some(asked), Some(window)) => Arc::ptr_eq(asked, window),
            _ => false,
        };
        if last {
            self.pending.remove(&id);
        }
        if let Some(old) = self.store.remove(&id) {
            self.bytes -= old.size();
            // A text that serves the window now isn't given up for one
            // that doesn't (a late result of an earlier ask).
            let serves = |made: &Made| {
                made.text
                    .as_ref()
                    .zip(self.window.as_ref())
                    .is_some_and(|((w, _), now)| w.covers(now))
            };
            if serves(&old) && !serves(&made) {
                made.text = old.text;
            }
        }
        let size = made.size();
        if self.bytes + size > self.limit {
            self.refused.insert(id);
            return;
        }
        self.bytes += size;
        self.store.insert(id, made);
    }

    /// The worker, started the first time one is needed; `None` when it
    /// couldn't be (the system's thread limits: the viewer goes on
    /// without anything made ahead).
    fn worker(&mut self) -> Option<&Worker> {
        if self.worker.is_none() {
            let wake = self.wake.clone()?;
            let (jobs, queue) = sync_channel(ASK_AT_ONCE);
            let (finished, done) = channel();
            let wanted = Arc::clone(&self.wanted);
            let builder = std::thread::Builder::new().name("sb-viewer-ahead".to_owned());
            let started = sb_cli_core::spawn::thread(builder, move || {
                work(&queue, &finished, &wanted, &wake);
            });
            let thread = match started {
                Ok(thread) => thread,
                Err(error) => {
                    tracing::debug!(%error, "no thread for the viewer's work ahead");
                    self.wake = None;
                    return None;
                }
            };
            self.worker = Some(Worker { jobs, done, thread });
        }
        self.worker.as_ref()
    }

    /// The bytes kept.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// The lines asked for and not yet back.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    /// The lines something is kept for.
    #[cfg(test)]
    pub(crate) fn kept(&self) -> usize {
        self.store.len()
    }

    /// The lines whose places the worker found from their start, so far.
    #[cfg(test)]
    pub(crate) fn fresh(&self) -> usize {
        self.fresh
    }

    /// Wait for every line asked for (tests: the frame after waits for
    /// the worker as a person's next key would).
    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        while !self.pending.is_empty() {
            let Some(worker) = &self.worker else { return };
            match worker.done.recv_timeout(std::time::Duration::from_secs(60)) {
                Ok(done) => self.keep(done),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    self.ended();
                    return;
                }
                Err(timeout) => panic!("the worker finishes its lines: {timeout}"),
            }
        }
    }

    /// A smaller bound, for tests.
    #[cfg(test)]
    pub(crate) fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// `text`'s bytes, counted as its lines, their spans and the spans'
/// contents: what keeping it costs, less the allocator's own.
pub(crate) fn text_bytes(text: &Text<'_>) -> usize {
    text.lines
        .iter()
        .map(|line| {
            let spans: usize = line
                .spans
                .iter()
                .map(|span| size_of::<Span<'_>>() + span.content.len())
                .sum();
            size_of::<TextLine<'_>>() + spans
        })
        .sum()
}

/// The worker: each job still wanted made and sent back, until the viewer
/// drops its sender (or its receiver).
fn work(
    queue: &Receiver<Job>,
    finished: &Sender<Done>,
    wanted: &Mutex<HashSet<u64>>,
    wake: &Notify,
) {
    for job in queue {
        if !wanted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&job.id)
        {
            continue;
        }
        let done = Done {
            id: job.id,
            window: job.window.clone(),
            #[cfg(test)]
            fresh: job.resumes.is_none(),
            made: make(job),
        };
        if finished.send(done).is_err() {
            return;
        }
        wake.notify_one();
    }
}

/// A job's line's places through its whole message, from those it has,
/// and its text for the job's window.
fn make(job: Job) -> Made {
    #[cfg(test)]
    assert_ne!(job.id, tests::PANICS, "a test's panic on the worker");
    let shown = line::shown(&job.line, &job.format).0;
    let mut resumes = job
        .resumes
        .unwrap_or_else(|| Resumes::of(shown, job.format.colour()));
    resume::scan(shown, &mut resumes);
    let text = job.window.and_then(|w| {
        resume::far(
            &job.line,
            &w.format,
            &w.query,
            w.format.hit(),
            &mut resumes,
            (w.left, w.reach),
            w.width,
        )
        .map(|far| (w, far))
    });
    Made { resumes, text }
}

#[cfg(test)]
mod tests;
