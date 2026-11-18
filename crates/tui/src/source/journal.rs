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
//!   documented loop of `sb_host::journal`'s module docs; a 4/s tick
//!   drains as well, so a read the follow waited on is retried without
//!   the journal's next write (follow-L1).
//! - **A follow that can't start** is a notice, `can't follow: <cause>; f
//!   retries`, the view staying paged; the cause names the system's
//!   inotify limit when that is what stopped it (follow-L4).
//! - `Journal::follow` blocks (the watches and the first batch), so it
//!   runs under tokio's `spawn_blocking`
//!   (<https://docs.rs/tokio/1.54.1/tokio/task/fn.spawn_blocking.html>),
//!   as sb-host's docs ask.
//!
//! The run log gets each page (direction, count, `more_*`, time taken),
//! follow start and stop, and every read error; never a log's content
//! (tui spec, "The run log").

use std::collections::{HashSet, VecDeque};
use std::ops::ControlFlow;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt as _;

use rustix::io::Errno;
use sb_cli_core::exit::chain;
use sb_host::journal::{
    self, Batch, Cursor, FollowWait, Follower, Journal, Limits, OpenError, PageEnd, Query,
    ReadErrorKind, ReadErrors, Record, Uid, UnitName, Wake,
};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::Instrument as _;

use super::{
    Event, Found, Inner, Line, Matcher, Notice, Order, Outbox, PageKind, Position, Request, Search,
    SearchEnd, Side, SourceHandle,
};

/// The page size: the tail's 500 lines (tui spec, "Opening"), and each
/// older and newer page.
const PAGE: usize = 500;

/// The output limit per page and per drain (tui decision 12; the Task 11
/// audit, san-I4), not sb-journal's 256 MiB default.
const OUTPUT: usize = 4 << 20;

/// The most cursors the cycle guard remembers: the viewer's buffer of
/// 20,000 entries (tui decision 12).
const REMEMBERED: usize = 20_000;

/// How often a follow drains with no wake: the screen's 4/s tick
/// (follow-L1).
const RETRY: Duration = Duration::from_millis(250);

/// A search's step: the entries one scan step reads and matches, and its
/// output limit (tui spec, "Whole-log search", "Journal").
const SEARCH_STEP: usize = 5_000;
const SEARCH_OUTPUT: usize = 16 << 20;

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
    /// How often a follow drains with no wake (follow-L1).
    retry: Duration,
    /// A search's step, its output limit, and its cap of matches a side.
    search_step: usize,
    search_limits: Limits,
    matches: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            page: PAGE,
            limits: Limits::with_output(OUTPUT),
            remembered: REMEMBERED,
            retry: RETRY,
            search_step: SEARCH_STEP,
            search_limits: Limits::with_output(SEARCH_OUTPUT),
            matches: super::MATCHES,
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
            search: None,
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
    /// The tick, while following.
    Retry,
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
    /// The search in progress: its token, cancelled when the guard drops
    /// (a new search, its stop, or the source's drop; tokio-util's
    /// `DropGuard`,
    /// <https://docs.rs/tokio-util/0.7.20/tokio_util/sync/struct.CancellationToken.html#method.drop_guard>),
    /// which the scans see at their next step.
    search: Option<DropGuard>,
}

impl Adapter {
    /// Requests and the follow's wakes until the viewer goes. A request
    /// is taken before a wake that is ready at the same time, so a busy
    /// follow can't hold one off between wakes; within one, a request
    /// waits for the drains to catch up (each is awaited to its end, as
    /// sb-host's `drain` asks).
    ///
    /// While following, a tick drains too (follow-L1): a read the follow
    /// waited on (an entry journald was still writing) is read again then,
    /// not only at the journal's next write, which may be a long time
    /// coming for a quiet service. With nothing new, a drain is a header
    /// read per open file (sb-journal's `Follower::drain`). tokio's
    /// `Interval` with `MissedTickBehavior::Delay`
    /// (<https://docs.rs/tokio/1.54.1/tokio/time/struct.Interval.html>),
    /// `reset` after every request and wake, so a read waited on at a
    /// drain gets a whole period before it is read again (sb-journal
    /// waits one drain on it) and a busy follow isn't drained twice;
    /// `tick` is cancel-safe, so it races in `select!`.
    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
        let mut retry = tokio::time::interval(self.settings.retry);
        retry.set_missed_tick_behavior(MissedTickBehavior::Delay);
        retry.reset();
        loop {
            let step = match self.follow.as_mut() {
                Some(following) => tokio::select! {
                    biased;
                    request = requests.recv() => Step::Request(request),
                    wake = following.waiter.wait() => Step::Woken(wake),
                    _ = retry.tick() => Step::Retry,
                },
                None => Step::Request(requests.recv().await),
            };
            let ticked = matches!(step, Step::Retry);
            let open = match step {
                Step::Request(None) => false,
                Step::Request(Some(request)) => self.handle(request).await,
                Step::Woken(Ok(Wake::Woken)) | Step::Retry => self.drain().await,
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
            if !ticked {
                retry.reset();
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
            Request::Around(Position(Inner::Journal(cursor))) => self.around(cursor).await,
            // Another source's position: none of this one's entries.
            Request::Older(_) | Request::Newer(_) | Request::Around(_) => true,
            Request::Search(search) => {
                self.start_search(search);
                true
            }
            Request::StopSearch => {
                self.stop_search();
                true
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

    /// The entry at `cursor` with its surroundings (`Request::Around`): the
    /// page ending with it (`AtOrBefore`) and the one after it, as one
    /// page that starts a new buffer, so the follow ends and the cycle
    /// guard forgets what it sent, as for a tail. A cursor that has gone
    /// (rotated away) still reads the entries on either side of where it
    /// was.
    async fn around(&mut self, cursor: Cursor) -> bool {
        self.stop_following();
        self.sent.clear();
        let started = Instant::now();
        let at_or_before = self.settings.page / 2 + 1;
        let after_n = self.settings.page.saturating_sub(at_or_before);
        let before = journal::page(
            Arc::clone(&self.journal),
            self.query.clone(),
            journal::Position::AtOrBefore(cursor),
            at_or_before,
            self.settings.limits,
        )
        .await;
        let after = journal::page(
            Arc::clone(&self.journal),
            self.query.clone(),
            journal::Position::After(cursor),
            after_n,
            self.settings.limits,
        )
        .await;
        if !after.more_newer {
            self.follow_from = Some(after.end.clone());
        }
        let records: Vec<&Record> = before.records.iter().chain(&after.records).collect();
        for r in &records {
            self.sent.remember(r.cursor());
        }
        tracing::info!(
            direction = ?PageKind::Around,
            count = records.len(),
            more_older = before.more_older,
            more_newer = after.more_newer,
            millis = started.elapsed().as_millis(),
            "journal page"
        );
        let event = Event::Page {
            kind: PageKind::Around,
            lines: records.iter().map(|r| line(r)).collect(),
            oldest: before.oldest_chosen.or(after.oldest_chosen).map(position),
            newest: after.newest_chosen.or(before.newest_chosen).map(position),
            more_older: before.more_older,
            more_newer: after.more_newer,
        };
        self.outbox.send(event) && self.errors(&before.errors) && self.errors(&after.errors)
    }

    /// A search's sides, each one scan on the blocking pool in a task of
    /// its own (`scan_side`), beside this task's pages and follow, so
    /// neither waits for the other. The search before it stops.
    fn start_search(&mut self, search: Search) {
        self.stop_search();
        let stop = CancellationToken::new();
        self.search = Some(stop.clone().drop_guard());
        tracing::info!(search = search.id, "journal search started");
        for (side, from) in [(Side::Older, search.older), (Side::Newer, search.newer)] {
            let Some(Position(Inner::Journal(cursor))) = from else {
                continue;
            };
            let side = SideScan {
                journal: Arc::clone(&self.journal),
                query: self.query.clone(),
                side,
                from: cursor,
                search: search.id,
                matcher: search.matcher.clone(),
                stop: stop.clone(),
                outbox: self.outbox.clone(),
                settings: self.settings,
            };
            tokio::spawn(side.run().in_current_span());
        }
    }

    fn stop_search(&mut self) {
        self.search = None;
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
        // The run's span goes along (cli-core's `spawn`).
        let started = sb_cli_core::spawn::blocking(move || journal.follow(query, &end, &limits))
            .await
            .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()));
        let (follower, batch) = match started {
            Ok(started) => started,
            Err(err) => {
                // `f` retries from the same end.
                self.follow_from = Some(from);
                let cause = cant_follow(&err);
                tracing::warn!(error = %cause, "journal follow failed to start");
                return self.outbox.send(Event::Notice(Notice::CantFollow(cause)));
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
        let shown: Vec<String> = shown.into_iter().map(|e| with_causes(e)).collect();
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

/// One side of a search: the entries before (or after) `from`, matched
/// a scan step at a time on the blocking pool, each step's matches sent
/// as an [`Event::Found`].
struct SideScan {
    journal: Arc<Journal>,
    query: Query,
    side: Side,
    from: Cursor,
    search: u64,
    matcher: Matcher,
    stop: CancellationToken,
    outbox: Outbox,
    settings: Settings,
}

impl SideScan {
    /// The scan walks newest first: the older side's matches come nearest
    /// the buffer first, and stop at the cap; the newer side's farthest
    /// first, all of them, the viewer keeping the nearest (tui spec,
    /// "Matches kept"). A read error in a step is logged, and the scan
    /// goes on, as a page's notice doesn't end paging: the errors are one
    /// notice, the first with the count of the others, before the side's
    /// end. A scan that panics ends its side as failed. The scan stops at the step after
    /// the stop flag is set or the viewer has gone.
    async fn run(self) {
        let Self {
            journal,
            query,
            side,
            from,
            search,
            matcher,
            stop,
            outbox,
            settings,
        } = self;
        let (at, order) = match side {
            Side::Older => (journal::Position::Before(from), Order::NearestFirst),
            Side::Newer => (journal::Position::After(from), Order::FarthestFirst),
        };
        let started = Instant::now();
        let (step_outbox, step_stop) = (outbox.clone(), stop.clone());
        let failures = Arc::new(Failures::default());
        let step_failures = Arc::clone(&failures);
        let mut kept = 0;
        let each = move |records: Vec<Record>, errors: ReadErrors| {
            if step_stop.is_cancelled() {
                return ControlFlow::Break(());
            }
            step_failures.note(&errors);
            let mut found = Vec::new();
            let mut reached = None;
            for record in &records {
                let line = line(record);
                reached = line.time.or(reached);
                let count = matcher.count(&line);
                if count > 0 {
                    found.push(Found {
                        position: line.position,
                        time: line.time,
                        count,
                    });
                }
            }
            let mut end = None;
            if order == Order::NearestFirst && kept + found.len() >= settings.matches {
                found.truncate(settings.matches - kept);
                end = Some(SearchEnd::Capped);
            }
            kept += found.len();
            let capped = end.is_some();
            let event = Event::Found {
                search,
                side,
                order,
                found,
                reached,
                end,
            };
            if !step_outbox.send(event) || capped {
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        };
        let scanned = AssertUnwindSafe(journal::scan(
            journal,
            query,
            at,
            settings.search_step,
            settings.search_limits,
            each,
        ))
        .catch_unwind()
        .await;
        let end = match scanned {
            Ok(ControlFlow::Continue(())) => SearchEnd::Done,
            Ok(ControlFlow::Break(())) => {
                tracing::info!(
                    search,
                    ?side,
                    millis = started.elapsed().as_millis(),
                    "journal search side stopped"
                );
                return;
            }
            Err(_) => SearchEnd::Failed("the search failed".to_string()),
        };
        tracing::info!(search, ?side, millis = started.elapsed().as_millis(), outcome = ?end, "journal search side ended");
        if !stop.is_cancelled() {
            if let Some(notice) = failures.notice() {
                let _ = outbox.send(Event::Notice(notice));
            }
            let _ = outbox.send(Event::Found {
                search,
                side,
                order,
                found: Vec::new(),
                reached: None,
                end: Some(end),
            });
        }
    }
}

/// A search side's read errors: the first, with its causes, and how
/// many, for one notice at its end.
#[derive(Debug, Default)]
struct Failures(std::sync::Mutex<(Option<String>, usize)>);

impl Failures {
    /// A step's errors: logged, and kept for the notice.
    fn note(&self, errors: &ReadErrors) {
        if errors.is_empty() {
            return;
        }
        let count = errors.shown.len() + errors.omitted;
        let first = errors.shown.first().map(|e| with_causes(e));
        tracing::warn!(
            errors = count,
            first = %first.clone().unwrap_or_default(),
            "journal search read errors"
        );
        let mut failed = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failed.0.is_none() {
            failed.0 = first;
        }
        failed.1 += count;
    }

    /// The notice: the first error, with the count of the others.
    fn notice(&self) -> Option<Notice> {
        let failed = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let first = failed.0.clone()?;
        Some(Notice::ReadErrors {
            first,
            others: failed.1.saturating_sub(1),
        })
    }
}

/// `err` and its sources, `: ` between them: an `Io` error's system
/// error is its source, not part of its text (sb-journal's `ReadError`).
/// cli-core's `exit::chain` walks them.
fn with_causes(err: &(dyn std::error::Error + 'static)) -> String {
    chain(err)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// Why a follow couldn't start (follow-L4; tui spec, "Sources"): `err`
/// and its causes, the system's error among them, and the limit's name
/// when it is the system's inotify limit: `EMFILE` creating the instance
/// (`fs.inotify.max_user_instances`, or the open files limit, which the
/// logs commands raise) and `ENOSPC` adding a watch
/// (`fs.inotify.max_user_watches`), as `inotify_init(2)` and
/// `inotify_add_watch(2)` give them
/// (<https://man7.org/linux/man-pages/man7/inotify.7.html>, "/proc
/// interfaces"). The errno is read with rustix's `Errno::from_io_error`
/// (<https://docs.rs/rustix/1.1.6/rustix/io/struct.Errno.html#method.from_io_error>).
fn cant_follow(err: &OpenError) -> String {
    let limit = match err {
        OpenError::Inotify { source } if Errno::from_io_error(source) == Some(Errno::MFILE) => {
            Some("fs.inotify.max_user_instances")
        }
        OpenError::Watch { source, .. } if Errno::from_io_error(source) == Some(Errno::NOSPC) => {
            Some("fs.inotify.max_user_watches")
        }
        _ => None,
    };
    let cause = with_causes(err);
    match limit {
        Some(limit) => format!("{cause}; the system's inotify limit ({limit}) may be reached"),
        None => cause,
    }
}

fn position(cursor: Cursor) -> Position {
    Position(Inner::Journal(cursor))
}

/// A record as a line: its time, journalctl's origin, its message.
fn line(record: &Record) -> Line {
    Line::new(
        record.realtime(),
        &origin(record),
        record.message().unwrap_or_default(),
        position(record.cursor()),
    )
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
