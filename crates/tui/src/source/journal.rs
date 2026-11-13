//! The journal adapter (tui spec, "Sources", Services; research
//! decisions 6 to 8): one source task per service, on sb-host's journal
//! API.
//!
//! - One query per source, [`Journal::unit_query`], so every page and the
//!   follow use the same query (tui decision 17; a follow's `from` is the
//!   `end` of a page of the same query).
//! - The tail is `Position::Tail` with n = 500; older is
//!   `Before(oldest_chosen)`, newer `After(newest_chosen)`, through
//!   sb-host's `journal::page`; each under `Limits::with_output(4 MiB)`
//!   (tui decision 12).
//! - **The cycle guard** (tui decision 11; journal Difference 20): a
//!   forward page whose records the source has all sent already adds
//!   nothing and ends forward paging (`more_newer` false, shown as `end
//!   of log`). The source remembers the cursors it has sent from the
//!   tail, newer pages and the follow, first in first out, at most the
//!   viewer's 20,000 entries, and forgets them on `Tail` and `Older`, and
//!   those sent after the edge of a `Newer` that isn't the newest sent
//!   (the viewer dropped its newest end while following).
//!   Why `Older` too: paging back is when the viewer drops its newest
//!   end (by entries, or sooner by its 32 MiB of bytes), and the newer
//!   pages that load that end again must not read as a cycle. Within a
//!   run of newer pages and follow batches the viewer drops only its
//!   oldest end, which no forward page returns but in a cycle. The cost:
//!   a cycle back into entries an older page loaded shows them once more
//!   before the guard stops it, at most one lap.
//! - **The follow** starts from the end of the page that holds the newest
//!   entry (the tail, or a forward page with nothing newer), then runs
//!   sb-host's wake protocol (`FollowWait`, then `drain` while
//!   `Batch::more`, and once more after a batch with errors), the
//!   documented loop of `sb_host::journal`'s module docs.
//! - `Journal::follow` blocks (the watches and the first batch), so it
//!   runs under tokio's `spawn_blocking`
//!   (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>),
//!   as sb-host's docs ask.
//!
//! The run log gets each page (direction, count, `more_*`, time taken),
//! follow start and stop, and every read error; never a log's content
//! (tui spec, "The run log").

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use sb_cli_core::exit::chain;
use sb_host::journal::{
    self, Batch, Cursor, FollowWait, Follower, Journal, Limits, PageEnd, Query, ReadError,
    ReadErrorKind, ReadErrors, Record, Uid, UnitName, Wake,
};
use tokio::sync::mpsc;

use super::{Event, Inner, Line, Notice, Outbox, PageKind, Position, Request, SourceHandle};

/// The page size: the tail's 500 lines (tui spec, "Opening"), and each
/// older and newer page.
const PAGE: usize = 500;

/// The output limit per page and per drain (tui decision 12; the Task 11
/// audit, san-I4), not sb-journal's 256 MiB default.
const OUTPUT: usize = 4 << 20;

/// The most cursors the cycle guard remembers: the viewer's buffer of
/// 20,000 entries (tui decision 12).
const REMEMBERED: usize = 20_000;

/// A source task for `unit`'s journal: one [`Journal::unit_query`] for
/// every page and the follow. `journal` is the run's one journal, opened
/// under `spawn_blocking`.
#[must_use]
pub fn spawn(
    journal: Arc<Journal>,
    unit: &UnitName,
    coredump_uid: Option<Uid>,
    outbox: Outbox,
) -> SourceHandle {
    let query = journal.unit_query(unit, coredump_uid);
    spawn_query(journal, query, Settings::default(), outbox)
}

/// The adapter's sizes; the tests make them small.
#[derive(Clone, Copy, Debug)]
struct Settings {
    page: usize,
    limits: Limits,
    remembered: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            page: PAGE,
            limits: Limits::with_output(OUTPUT),
            remembered: REMEMBERED,
        }
    }
}

fn spawn_query(
    journal: Arc<Journal>,
    query: Query,
    settings: Settings,
    outbox: Outbox,
) -> SourceHandle {
    SourceHandle::spawn(outbox, move |requests, outbox| {
        Adapter {
            journal,
            query,
            settings,
            outbox,
            follow_from: None,
            sent: Sent::new(settings.remembered),
            follow: None,
        }
        .run(requests)
    })
}

/// The cursors a source has sent, oldest sent first, at most `cap`. Why
/// not a crate's ordered set: none is a direct dependency, and the ones in
/// the lock don't fit (lru's `put` reorders a key sent again; indexmap
/// removes its first entry in O(n); hashlink would be a new dependency for
/// these lines).
#[derive(Debug)]
struct Sent {
    order: VecDeque<Cursor>,
    held: HashSet<Cursor>,
    cap: usize,
}

impl Sent {
    fn new(cap: usize) -> Self {
        Self {
            order: VecDeque::new(),
            held: HashSet::new(),
            cap,
        }
    }

    fn remember(&mut self, cursor: Cursor) {
        if self.cap == 0 || !self.held.insert(cursor) {
            return;
        }
        self.order.push_back(cursor);
        if self.order.len() > self.cap
            && let Some(old) = self.order.pop_front()
        {
            self.held.remove(&old);
        }
    }

    fn holds(&self, cursor: &Cursor) -> bool {
        self.held.contains(cursor)
    }

    /// Forget what was sent after `cursor`, when it was sent: the viewer
    /// asks for the page after a line it holds that isn't the newest sent,
    /// so it has dropped its newest end. `true` when anything was
    /// forgotten.
    fn forget_after(&mut self, cursor: &Cursor) -> bool {
        if !self.holds(cursor) {
            return false;
        }
        let mut forgot = false;
        while let Some(last) = self.order.back()
            && last != cursor
        {
            self.held.remove(last);
            self.order.pop_back();
            forgot = true;
        }
        forgot
    }

    fn clear(&mut self) {
        self.order.clear();
        self.held.clear();
    }
}

/// A follow in progress: the follower and its wait.
#[derive(Debug)]
struct Following {
    follower: Follower,
    waiter: FollowWait,
}

/// What the task woke for.
enum Step {
    Request(Option<Request>),
    Woken(std::io::Result<Wake>),
}

#[derive(Debug)]
struct Adapter {
    journal: Arc<Journal>,
    query: Query,
    settings: Settings,
    outbox: Outbox,
    /// The end of the last page that held the newest entry, until a
    /// follow starts from it.
    follow_from: Option<PageEnd>,
    sent: Sent,
    follow: Option<Following>,
}

impl Adapter {
    /// Requests and the follow's wakes until the viewer goes. A request
    /// is taken before a wake that is ready at the same time, so a busy
    /// follow can't hold one off between wakes; within one, a request
    /// waits for the drains to catch up (each is awaited to its end, as
    /// sb-host's `drain` asks).
    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
        loop {
            let step = match self.follow.as_mut() {
                Some(following) => tokio::select! {
                    biased;
                    request = requests.recv() => Step::Request(request),
                    wake = following.waiter.wait() => Step::Woken(wake),
                },
                None => Step::Request(requests.recv().await),
            };
            let open = match step {
                Step::Request(None) => false,
                Step::Request(Some(request)) => self.handle(request).await,
                Step::Woken(Ok(Wake::Woken)) => self.drain().await,
                Step::Woken(Ok(Wake::Stopped)) => {
                    self.stop_following();
                    true
                }
                Step::Woken(Err(err)) => {
                    self.stop_following();
                    tracing::warn!(error = %err, "journal follow wait failed");
                    self.outbox
                        .send(Event::Notice(Notice::CantFollow(err.to_string())))
                }
            };
            if !open {
                return;
            }
        }
    }

    /// `false` once the viewer is gone.
    async fn handle(&mut self, request: Request) -> bool {
        match request {
            Request::Tail => self.tail().await,
            Request::Older(Position(Inner::Journal(cursor))) => {
                self.page(PageKind::Older, journal::Position::Before(cursor))
                    .await
            }
            Request::Newer(Position(Inner::Journal(cursor))) => {
                if self.sent.forget_after(&cursor) {
                    // The page that held the newest entry is past the
                    // viewer's edge now: following from its end would
                    // leave a gap.
                    self.follow_from = None;
                }
                self.page(PageKind::Newer, journal::Position::After(cursor))
                    .await
            }
            Request::Follow(true) => self.start_following().await,
            Request::Follow(false) => {
                self.stop_following();
                true
            }
        }
    }

    /// The newest entries: a new buffer, so the follow ends and the cycle
    /// guard forgets what it sent.
    async fn tail(&mut self) -> bool {
        self.stop_following();
        self.sent.clear();
        self.page(PageKind::Tail, journal::Position::Tail).await
    }

    async fn page(&mut self, kind: PageKind, at: journal::Position) -> bool {
        let started = Instant::now();
        let page = journal::page(
            Arc::clone(&self.journal),
            self.query.clone(),
            at,
            self.settings.page,
            self.settings.limits,
        )
        .await;
        let mut more_newer = page.more_newer;
        let mut records = page.records;
        if kind == PageKind::Newer && !records.is_empty() {
            // The cycle guard (journal Difference 20).
            if records.iter().all(|r| self.sent.holds(&r.cursor())) {
                records.clear();
                more_newer = false;
                tracing::info!("journal forward page returned to shown entries; paging ends");
            }
        }
        if kind == PageKind::Tail || (kind == PageKind::Newer && !page.more_newer) {
            self.follow_from = Some(page.end);
        }
        tracing::info!(
            direction = ?kind,
            count = records.len(),
            more_older = page.more_older,
            more_newer,
            millis = started.elapsed().as_millis(),
            "journal page"
        );
        if kind == PageKind::Older {
            self.sent.clear();
        } else {
            for r in &records {
                self.sent.remember(r.cursor());
            }
        }
        let event = Event::Page {
            kind,
            lines: records.iter().map(line).collect(),
            oldest: page.oldest_chosen.map(position),
            newest: page.newest_chosen.map(position),
            more_older: page.more_older,
            more_newer,
        };
        self.outbox.send(event) && self.errors(&page.errors)
    }

    /// Follow from the last page that held the newest entry, reading the
    /// tail first when there is none.
    async fn start_following(&mut self) -> bool {
        if self.follow.is_some() {
            return true;
        }
        if self.follow_from.is_none() && !self.tail().await {
            return false;
        }
        let Some(from) = self.follow_from.take() else {
            return true;
        };
        let (journal, query, limits) = (
            Arc::clone(&self.journal),
            self.query.clone(),
            self.settings.limits,
        );
        let end = from.clone();
        // A panic in the follow is resumed here, as sb-host's helpers do
        // (<https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinError.html#method.into_panic>).
        let started = tokio::task::spawn_blocking(move || journal.follow(query, &end, &limits))
            .await
            .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
        let (follower, batch) = match started {
            Ok(started) => started,
            Err(err) => {
                // `f` retries from the same end.
                self.follow_from = Some(from);
                tracing::warn!(error = %err, "journal follow failed to start");
                return self
                    .outbox
                    .send(Event::Notice(Notice::CantFollow(err.to_string())));
            }
        };
        let waiter = match FollowWait::new(&follower) {
            Ok(waiter) => waiter,
            Err(err) => {
                self.follow_from = Some(from);
                tracing::warn!(error = %err, "journal follow wait failed to start");
                return self
                    .outbox
                    .send(Event::Notice(Notice::CantFollow(err.to_string())));
            }
        };
        tracing::info!("journal follow started");
        let more = batch.more;
        let errors = !batch.errors.is_empty();
        if !self.followed(&batch) {
            return false;
        }
        self.follow = Some(Following { follower, waiter });
        if more || errors {
            // The first batch was cut short: drain on before waiting.
            return self.drain().await;
        }
        true
    }

    fn stop_following(&mut self) {
        if self.follow.take().is_some() {
            tracing::info!("journal follow stopped");
        }
    }

    /// One wake of the follow: drain while `Batch::more`, and once more
    /// after a batch with errors (sb-host's wake protocol).
    async fn drain(&mut self) -> bool {
        let Some(Following {
            mut follower,
            waiter,
        }) = self.follow.take()
        else {
            return true;
        };
        let mut retried = false;
        loop {
            let batch;
            (follower, batch) = journal::drain(follower, self.settings.limits).await;
            let (more, errors) = (batch.more, !batch.errors.is_empty());
            if !self.followed(&batch) {
                return false;
            }
            if more {
                continue;
            }
            if errors && !retried {
                retried = true;
                continue;
            }
            break;
        }
        self.follow = Some(Following { follower, waiter });
        true
    }

    /// A follow's batch to the viewer: its lines, when there are any, and
    /// its errors.
    fn followed(&mut self, batch: &Batch) -> bool {
        if !batch.records.is_empty() {
            for r in &batch.records {
                self.sent.remember(r.cursor());
            }
            let lines = batch.records.iter().map(line).collect();
            if !self.outbox.send(Event::Followed(lines)) {
                return false;
            }
        }
        self.errors(&batch.errors)
    }

    /// Read errors: every one to the run log, the first to the viewer
    /// with the count of the others, each with its causes. The limits that
    /// end a page aren't errors to show: the page's `more_*` count the
    /// entries they left out, and the next page reads them (sb-journal's
    /// `Page`), so they are logged only.
    fn errors(&self, errors: &ReadErrors) -> bool {
        let (ended, shown): (Vec<_>, Vec<_>) = errors.shown.iter().partition(|e| {
            matches!(
                e.kind(),
                ReadErrorKind::OutputLimit { .. }
                    | ReadErrorKind::OutputLimitNewer { .. }
                    | ReadErrorKind::HeldLimit
                    | ReadErrorKind::HeldLimitNewer
            )
        });
        for err in &ended {
            tracing::info!(error = %err, "journal page ended by its limit");
        }
        let shown: Vec<String> = shown.into_iter().map(with_causes).collect();
        for err in &shown {
            tracing::warn!(error = %err, "journal read error");
        }
        if errors.omitted > 0 {
            tracing::warn!(omitted = errors.omitted, "more journal read errors");
        }
        let Some(first) = shown.first() else {
            return true;
        };
        let notice = Notice::ReadErrors {
            first: first.clone(),
            others: shown.len() - 1 + errors.omitted,
        };
        self.outbox.send(Event::Notice(notice))
    }
}

/// `err` and its sources, `: ` between them: an `Io` error's system
/// error is its source, not part of its text (sb-journal's `ReadError`).
/// cli-core's `exit::chain` walks them.
fn with_causes(err: &ReadError) -> String {
    chain(err)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn position(cursor: Cursor) -> Position {
    Position(Inner::Journal(cursor))
}

/// A record as a line: its time, journalctl's origin, its message.
fn line(record: &Record) -> Line {
    Line {
        time: record.realtime(),
        origin: origin(record),
        message: record.message().unwrap_or_default().to_vec(),
        position: position(record.cursor()),
    }
}

/// journalctl's origin (its `short` output): `SYSLOG_IDENTIFIER`, else
/// `_COMM`, with `[SYSLOG_PID]`, else `[_PID]`, when either is set.
fn origin(record: &Record) -> Vec<u8> {
    let (mut ident, mut comm, mut syslog_pid, mut pid) = (None, None, None, None);
    for (name, value) in record.fields() {
        let slot = match name {
            b"SYSLOG_IDENTIFIER" => &mut ident,
            b"_COMM" => &mut comm,
            b"SYSLOG_PID" => &mut syslog_pid,
            b"_PID" => &mut pid,
            _ => continue,
        };
        slot.get_or_insert(value);
    }
    let mut origin = ident.or(comm).unwrap_or_default().to_vec();
    if let Some(pid) = syslog_pid.or(pid) {
        origin.push(b'[');
        origin.extend_from_slice(pid);
        origin.push(b']');
    }
    origin
}

#[cfg(test)]
mod tests;
