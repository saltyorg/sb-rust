//! The log-source protocol (tui spec, "Sources"; research decision 7;
//! tui decision 10): each open source is a task that takes [`Request`]s
//! and sends [`Event`]s, each tagged with the source's [`Generation`], so
//! the viewer drops the events of a source the user has left. A channel,
//! not a trait: the viewer runs against a fake source task in tests
//! (`testing`), and the journal and docker adapters need no shared async
//! trait object.
//!
//! The wake-up path: a source sends on the viewer's unbounded channel,
//! then calls `notify_one` on the screen's `Arc<tokio::sync::Notify>`
//! ([`Outbox`]). `Notify` keeps one permit when nothing waits
//! (<https://docs.rs/tokio/1.54.1/tokio/sync/struct.Notify.html#method.notify_one>),
//! so a wake is never lost and a burst of events costs one wake; the
//! screen then takes every event waiting with `try_recv`.
//!
//! The channels are tokio's `mpsc::unbounded_channel`
//! (<https://docs.rs/tokio/1.54.1/tokio/sync/mpsc/fn.unbounded_channel.html>):
//! the viewer asks for one page at a time per direction, and each of a
//! follow's batches is bounded by its output limit (a backlog comes as
//! several), so the queue stays short while the viewer keeps up.

pub mod docker;
mod journal;

use std::fmt;
use std::sync::Arc;

use jiff::Timestamp;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;

pub use docker::spawn as spawn_docker;
pub use journal::spawn as spawn_journal;

/// Which opening of a source an event belongs to: each source the viewer
/// opens gets the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(u64);

impl Generation {
    /// The generation after this one.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Where a line is in its source: opaque to the viewer, which hands an
/// edge's position back in [`Request::Older`] and [`Request::Newer`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Position(pub(crate) Inner);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Inner {
    /// A journal entry, by its cursor.
    Journal(sb_host::journal::Cursor),
    /// A container's log entry, by its time and ordinal.
    Docker(sb_docker::logs::Position),
}

/// One entry of a log, as raw as its source gives it: the viewer turns
/// the bytes into screen text (`DisplayText`, journal spec "Display").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// When the entry was written; `None` when the source doesn't say.
    pub time: Option<Timestamp>,
    /// Who wrote it: for a service journalctl's `ident[pid]`, for a
    /// container the stream; empty when unknown. Untrusted bytes.
    pub origin: Vec<u8>,
    /// The message, untrusted bytes, held to the record cap.
    pub message: Vec<u8>,
    /// The message's bytes past the record cap, not held: the viewer's
    /// `… [N bytes not shown]` counts them.
    pub cut: usize,
    /// Where it is.
    pub position: Position,
}

impl Line {
    /// A source's entry as a line: at most [`crate::line::RECORD_CAP`] of
    /// its message and [`crate::line::ORIGIN_CAP`] of its origin are
    /// copied, and the message's rest counted in `cut`. Nothing shows,
    /// searches, saves or prints past those caps, and a field many
    /// entries share is stored once in a journal file and charged once by
    /// a page's 4 MiB output limit, while each line holds its own copy
    /// (M73's security pass): so a page holds at most 500 × (64 KiB +
    /// 256 B).
    #[must_use]
    pub fn new(time: Option<Timestamp>, origin: &[u8], message: &[u8], position: Position) -> Self {
        let held = message.len().min(crate::line::RECORD_CAP);
        Self {
            time,
            origin: origin[..origin.len().min(crate::line::ORIGIN_CAP)].to_vec(),
            message: message[..held].to_vec(),
            cut: message.len() - held,
            position,
        }
    }
}

/// What the viewer asks of a source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// The newest entries. It ends a follow, and the source forgets the
    /// lines it sent (a new buffer starts).
    Tail,
    /// The page before this position: the oldest edge of the viewer's
    /// buffer ([`Event::Page`]'s `oldest`, or a line's position after
    /// the buffer dropped its old end).
    Older(Position),
    /// The page after this position: the newest edge, as for `Older`.
    /// The source's cycle guard ends forward paging on a page of lines it
    /// has sent already, so after dropping its newest end the viewer asks
    /// from its new edge (a line it still holds), never from an older one.
    Newer(Position),
    /// Follow (`true`) or stop following. A follow starts from the end of
    /// the last `Tail`, or `Newer` page with nothing newer, since the
    /// last follow; with none, the source reads the tail first (an
    /// [`Event::Page`] of [`PageKind::Tail`]), as `f` with the newest line
    /// not loaded does (tui spec, "Follow"). A viewer that has dropped its
    /// newest end sends `Tail` or pages `Newer` to the end before it
    /// follows; a `Newer` from below the newest line sent already makes
    /// the next follow read the tail first.
    Follow(bool),
    /// The entry at this position with its surroundings: up to 250
    /// entries before it, it, and up to 249 after (tui spec, "Whole-log
    /// search"), as an [`Event::Page`] of [`PageKind::Around`]. It ends a
    /// follow, and the source forgets the lines it sent, as `Tail` does.
    Around(Position),
    /// Search the log outside the viewer's buffer (tui spec, "Whole-log
    /// search"), stopping any search before it.
    Search(Search),
    /// Stop the search in progress, if any.
    StopSearch,
    /// The log's first entries (tui spec, "Home and End"), as an
    /// [`Event::Page`] of [`PageKind::Head`]. It ends a follow, and the
    /// source forgets the lines it sent, as `Tail` does.
    Head,
}

/// The most matches a side of a search keeps (tui spec, "Matches
/// kept"): the sources stop a side sending nearest first there, and the
/// viewer keeps the nearest of a side sending farthest first.
pub const MATCHES: usize = 10_000;

/// [`MATCHES`] as the header writes it.
pub const MATCHES_TEXT: &str = "10,000";

/// A whole-log search: its id, the viewer's match, and where each side
/// starts (`None`: that side isn't read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    /// The viewer's count of its searches: [`Event::Found`] carries it,
    /// so the viewer drops what an earlier search sends.
    pub id: u64,
    pub matcher: Matcher,
    /// Read the entries before this position (the buffer's oldest edge).
    pub older: Option<Position>,
    /// Read the entries after this position (the buffer's newest edge).
    pub newer: Option<Position>,
}

/// How many matches of the viewer's search a line holds, as the screen
/// shows it (`line::hits`): the same test inside the buffer and out.
/// Compared by identity.
#[derive(Clone)]
pub struct Matcher(Arc<dyn Fn(&Line) -> usize + Send + Sync>);

impl Matcher {
    #[must_use]
    pub fn new(matches: impl Fn(&Line) -> usize + Send + Sync + 'static) -> Self {
        Self(Arc::new(matches))
    }

    /// The matches in `line`.
    #[must_use]
    pub fn count(&self, line: &Line) -> usize {
        (self.0)(line)
    }
}

impl fmt::Debug for Matcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Matcher")
    }
}

impl PartialEq for Matcher {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Matcher {}

/// Which side of the buffer a search reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Older,
    Newer,
}

/// The order a side's matches come in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// Nearest the buffer first: the side stops itself at its cap.
    NearestFirst,
    /// Farthest from the buffer first: the viewer keeps the nearest.
    FarthestFirst,
}

/// A match outside the buffer: where its entry is, when it was written,
/// and how many matches it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub position: Position,
    pub time: Option<Timestamp>,
    pub count: usize,
}

/// How a side of a search ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchEnd {
    /// It read to the end of the log on its side.
    Done,
    /// It stopped at its cap of matches ([`Order::NearestFirst`]).
    Capped,
    /// It couldn't read on: the cause.
    Failed(String),
}

/// Which request a page answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    /// The newest entries: the buffer starts again with these.
    Tail,
    /// Entries before the buffer.
    Older,
    /// Entries after the buffer.
    Newer,
    /// An entry and its surroundings ([`Request::Around`]): the buffer
    /// starts again with these, not following.
    Around,
    /// The log's first entries ([`Request::Head`]): the buffer starts
    /// again with these, not following.
    Head,
}

/// What a source sends the viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A page, oldest line first.
    Page {
        /// The request it answers.
        kind: PageKind,
        lines: Vec<Line>,
        /// The page's oldest edge: where the next `Older` starts. It can
        /// be past the oldest line, at an entry the page reached but
        /// couldn't show. `None` when the page reached none.
        oldest: Option<Position>,
        /// The page's newest edge, as `oldest`.
        newest: Option<Position>,
        /// Older entries exist (`false`: `--- start of log ---`).
        more_older: bool,
        /// Newer entries exist (`false`: `--- end of log ---` when not
        /// following).
        more_newer: bool,
    },
    /// New entries while following, oldest first.
    Followed(Vec<Line>),
    /// A step of a search's side: the matches it found, in `order`, the
    /// time of the last entry it read, and, at its last step, how it
    /// ended.
    Found {
        search: u64,
        side: Side,
        order: Order,
        found: Vec<Found>,
        reached: Option<Timestamp>,
        end: Option<SearchEnd>,
    },
    /// Something to show as its own row, never in a log line.
    Notice(Notice),
}

/// A viewer notice from a source (tui spec, "Viewer notices").
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Notice {
    /// The follow couldn't start; the view stays paged (follow-L4).
    CantFollow(String),
    /// A page or a follow's batch had read errors: the first, and how
    /// many more (each is in the run log).
    ReadErrors { first: String, others: usize },
    /// A page couldn't be read at all: its cause. Paging that way ends.
    ReadFailed(String),
    /// The followed container stopped; the follow waits for it to start
    /// again.
    Stopped,
    /// The container started again; the follow goes on.
    Started,
}

impl Notice {
    /// An error, drawn in the Problem role; the others in Muted (tui spec,
    /// "Viewer notices").
    #[must_use]
    pub fn is_error(&self) -> bool {
        !matches!(self, Self::Stopped | Self::Started)
    }

    /// The follow's own notice, drawn at the bottom whatever page came
    /// last.
    #[must_use]
    pub fn of_follow(&self) -> bool {
        matches!(self, Self::CantFollow(_) | Self::Stopped | Self::Started)
    }
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CantFollow(cause) => write!(f, "can't follow: {cause}; f retries"),
            Self::ReadErrors { first, others: 0 } => write!(f, "read error: {first}"),
            Self::ReadErrors { first, others } => {
                write!(f, "read error: {first} (and {others} more)")
            }
            Self::ReadFailed(cause) => write!(f, "couldn't read the log: {cause}"),
            Self::Stopped => f.write_str("container stopped; waiting for it to start again"),
            Self::Started => f.write_str("container started"),
        }
    }
}

/// An event and the generation of the source that sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tagged {
    pub generation: Generation,
    pub event: Event,
}

/// Where a source sends its events: the viewer's channel, the screen's
/// wake, and the generation every event is tagged with.
#[derive(Clone, Debug)]
pub struct Outbox {
    events: mpsc::UnboundedSender<Tagged>,
    wake: Arc<Notify>,
    generation: Generation,
}

impl Outbox {
    #[must_use]
    pub fn new(
        events: mpsc::UnboundedSender<Tagged>,
        wake: Arc<Notify>,
        generation: Generation,
    ) -> Self {
        Self {
            events,
            wake,
            generation,
        }
    }

    /// The generation its events carry.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// The same channel and wake, tagging with `generation`: an old
    /// source's events, for the tests.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub(crate) fn retagged(&self, generation: Generation) -> Self {
        Self {
            generation,
            ..self.clone()
        }
    }

    /// Send `event`, tagged, and wake the screen. `false` once the viewer
    /// is gone: the source then ends.
    #[must_use]
    pub fn send(&self, event: Event) -> bool {
        let sent = self
            .events
            .send(Tagged {
                generation: self.generation,
                event,
            })
            .is_ok();
        self.wake.notify_one();
        sent
    }
}

/// An open source: its request channel and its task. Dropping it ends
/// the task (the source's stream stopped and its follow freed, tui spec
/// "Leaving"), through tokio's `JoinHandle::abort`
/// (<https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinHandle.html#method.abort>).
/// Why not tokio-util's `AbortOnDropHandle`: it needs tokio-util's `rt`
/// feature, a workspace-wide change, for a three-line `Drop`.
#[derive(Debug)]
pub struct SourceHandle {
    requests: mpsc::UnboundedSender<Request>,
    generation: Generation,
    /// `None` for the fake source, whose requests the test takes.
    task: Option<JoinHandle<()>>,
}

impl SourceHandle {
    /// A source task running `run`, which takes its requests and an
    /// [`Outbox`] for `generation`.
    pub(crate) fn spawn<F, Fut>(outbox: Outbox, run: F) -> Self
    where
        F: FnOnce(mpsc::UnboundedReceiver<Request>, Outbox) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (requests, received) = mpsc::unbounded_channel();
        let generation = outbox.generation;
        let task = tokio::spawn(run(received, outbox));
        Self {
            requests,
            generation,
            task: Some(task),
        }
    }

    /// A handle with no task: the requests go to the receiver returned
    /// (the fake source, `testing`).
    #[cfg(any(test, feature = "testing"))]
    pub(crate) fn detached(generation: Generation) -> (Self, mpsc::UnboundedReceiver<Request>) {
        let (requests, received) = mpsc::unbounded_channel();
        let handle = Self {
            requests,
            generation,
            task: None,
        };
        (handle, received)
    }

    /// Ask the source for `request`; its answer comes as events. A source
    /// that has ended takes it and does nothing.
    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    /// The generation its events carry.
    #[must_use]
    pub fn generation(&self) -> Generation {
        self.generation
    }
}

impl Drop for SourceHandle {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The search's matcher compares by identity, and its debug output
    /// shows nothing of what it holds.
    #[test]
    fn matchers_are_compared_by_identity() {
        let matcher = Matcher::new(|_: &Line| 1);
        assert_eq!(matcher, matcher.clone());
        assert_ne!(matcher, Matcher::new(|_: &Line| 1));
        assert_eq!(format!("{matcher:?}"), "Matcher");
    }

    /// A line holds at most the record cap of its message and the origin
    /// cap of its origin, and counts the message bytes it cut (M73's
    /// security pass): a field many entries share is stored once in a
    /// journal file and charged once by a page's output limit, but each
    /// line holds its own copy.
    #[test]
    fn a_line_holds_at_most_its_caps() {
        let message = vec![b'm'; 1 << 20];
        let origin = vec![b'o'; 1000];
        let big = Line::new(None, &origin, &message, crate::testing::position(1));
        assert_eq!(big.message.len(), crate::line::RECORD_CAP);
        assert_eq!(big.cut, (1 << 20) - crate::line::RECORD_CAP);
        assert_eq!(big.origin.len(), crate::line::ORIGIN_CAP);
        let at = vec![b'm'; crate::line::RECORD_CAP];
        let small = Line::new(None, b"app[7]", &at, crate::testing::position(2));
        assert_eq!((small.message.len(), small.cut), (at.len(), 0));
        assert_eq!(small.origin, b"app[7]");
    }

    #[test]
    fn generations_follow_each_other() {
        let first = Generation::default();
        assert_ne!(first, first.next());
        assert!(first < first.next());
        assert_eq!(first.next(), first.next());
    }

    #[test]
    fn notices_say_what_happened() {
        let cant = Notice::CantFollow("inotify watch limit reached".into());
        assert_eq!(
            cant.to_string(),
            "can't follow: inotify watch limit reached; f retries"
        );
        let one = Notice::ReadErrors {
            first: "/j/x.journal: bad".into(),
            others: 0,
        };
        assert_eq!(one.to_string(), "read error: /j/x.journal: bad");
        let more = Notice::ReadErrors {
            first: "/j/x.journal: bad".into(),
            others: 3,
        };
        assert_eq!(
            more.to_string(),
            "read error: /j/x.journal: bad (and 3 more)"
        );
        let failed = Notice::ReadFailed("connection refused".into());
        assert_eq!(
            failed.to_string(),
            "couldn't read the log: connection refused"
        );
        assert_eq!(
            Notice::Stopped.to_string(),
            "container stopped; waiting for it to start again"
        );
        assert_eq!(Notice::Started.to_string(), "container started");
        // Errors and the follow's own notices.
        let all = [cant, more, failed, Notice::Stopped, Notice::Started];
        let errors: Vec<bool> = all.iter().map(Notice::is_error).collect();
        assert_eq!(errors, [true, true, true, false, false]);
        let follows: Vec<bool> = all.iter().map(Notice::of_follow).collect();
        assert_eq!(follows, [true, false, false, true, true]);
    }

    #[tokio::test]
    async fn an_outbox_tags_its_events_and_wakes_the_screen() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let wake = Arc::new(Notify::new());
        let generation = Generation::default().next();
        let outbox = Outbox::new(tx, Arc::clone(&wake), generation);
        assert!(outbox.send(Event::Followed(Vec::new())));
        assert!(outbox.send(Event::Followed(Vec::new())));
        // Two sends, one stored permit: the wait ends at once.
        tokio::time::timeout(std::time::Duration::from_secs(5), wake.notified())
            .await
            .expect("woken");
        let got = rx.try_recv().expect("an event");
        assert_eq!(got.generation, generation);
        assert_eq!(got.event, Event::Followed(Vec::new()));
        drop(rx);
        assert!(!outbox.send(Event::Followed(Vec::new())), "viewer gone");
    }

    #[tokio::test]
    async fn dropping_the_handle_ends_the_task() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let outbox = Outbox::new(tx, Arc::new(Notify::new()), Generation::default());
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = SourceHandle::spawn(outbox, |mut requests, _outbox| async move {
            // Held until the task is dropped.
            let _done = done_tx;
            while requests.recv().await.is_some() {}
            std::future::pending::<()>().await;
        });
        assert_eq!(handle.generation(), Generation::default());
        handle.send(Request::Tail);
        drop(handle);
        // The sender inside the aborted task is dropped with it.
        let ended = tokio::time::timeout(std::time::Duration::from_secs(5), done_rx).await;
        assert!(ended.expect("ended in time").is_err());
    }
}
