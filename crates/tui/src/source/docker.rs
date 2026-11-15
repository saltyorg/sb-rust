//! The docker adapter (tui spec, "Sources", Containers; decided.md A-Q2;
//! research decision 19): one source task per container, on sb-docker's
//! log reads, and the Containers section's live list ([`watch`]).
//!
//! - The tail is `tail=500`; older pages are sb-docker's time windows
//!   back to the container's creation (`start of log`), newer pages read
//!   from the newest position's second on. Positions are strictly
//!   ordered, so no cycle guard is needed.
//! - **The follow** starts from the end of the page that holds the newest
//!   entry (the tail, or a newer page with nothing newer), as the journal
//!   adapter's, through sb-docker's `logs::follow`, whose batches come
//!   with futures-util's `ready_chunks`
//!   (<https://docs.rs/futures-util/0.3.34/futures_util/stream/trait.StreamExt.html#method.ready_chunks>):
//!   every line ready at once, in one event.
//! - **Across restarts:** the daemon ends a follow when the container
//!   stops. The source then says so ([`Notice::Stopped`]) and waits for a
//!   `start` event for the container's name, which carries its ID (a
//!   re-created container has a new one), and follows on from the last
//!   position ([`Notice::Started`]). The events stream is open for the
//!   source's whole life, so a restart quicker than the end of the log
//!   stream isn't missed: a `start` seen while following, with no `die`
//!   after it, resumes the follow as soon as it ends. A stream that ends
//!   or fails is opened again after 5 s.
//!
//! The run log gets each page (direction, count, `more_*`, time taken),
//! follow start and stop, and every error; never a log's content (tui
//! spec, "The run log").

use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::OptionFuture;
use futures_util::{Stream, StreamExt};
use jiff::Timestamp;
use sb_docker::connect::{self, Daemon};
use sb_docker::events::{self, Action, ContainerEvent};
use sb_docker::logs::{self, LogLine, LogStream};
use sb_docker::ps::Row;
use tokio::sync::{Notify, mpsc, watch as channel};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, Sleep};

use super::{Event, Inner, Line, Notice, Outbox, PageKind, Position, Request, SourceHandle};

/// The page size: the tail's 500 lines (tui spec, "Opening"), and each
/// older and newer page.
const PAGE: usize = 500;

/// How long a stopped events stream waits before it is opened again (tui
/// spec, "Live updates").
const RETRY: Duration = Duration::from_secs(5);

/// The Containers section's reload tick: an event marks the list stale,
/// the next tick reloads it (tui spec, "Live updates": 4/s).
const TICK: Duration = Duration::from_millis(250);

/// Before every entry: a follow from here sends the whole log (an empty
/// tail's follow).
const START: logs::Position = logs::Position {
    time_ns: i128::MIN,
    ordinal: 0,
};

/// The container a source shows: its ID, its name (events name it so)
/// and its creation, where older pages stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub created: Option<Timestamp>,
}

/// A source task for `container`'s log on `daemon`.
#[must_use]
pub fn spawn(daemon: Arc<Daemon>, container: Container, outbox: Outbox) -> SourceHandle {
    spawn_with(daemon, container, Settings::default(), outbox)
}

/// The adapter's sizes and waits; the tests make them small.
#[derive(Clone, Copy, Debug)]
struct Settings {
    page: usize,
    retry: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            page: PAGE,
            retry: RETRY,
        }
    }
}

fn spawn_with(
    daemon: Arc<Daemon>,
    container: Container,
    settings: Settings,
    outbox: Outbox,
) -> SourceHandle {
    SourceHandle::spawn(outbox, move |requests, outbox| {
        let events = Some(open_events(&daemon));
        Adapter {
            daemon,
            container,
            settings,
            outbox,
            follow_from: None,
            follow: Follow::Off,
            restarted: None,
            tail_failed: false,
            events,
            retry: None,
        }
        .run(requests)
    })
}

type Lines = Pin<Box<dyn Stream<Item = Vec<anyhow::Result<LogLine>>> + Send>>;
type Events = Pin<Box<dyn Stream<Item = anyhow::Result<ContainerEvent>> + Send>>;

fn open_events(daemon: &Daemon) -> Events {
    Box::pin(events::container_events(daemon))
}

/// The follow: off, reading the log from a position on, or waiting for
/// the container to start again with the position to go on from.
enum Follow {
    Off,
    On { lines: Lines, at: logs::Position },
    Waiting { at: logs::Position },
}

impl std::fmt::Debug for Follow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::On { at, .. } => f.debug_struct("On").field("at", at).finish_non_exhaustive(),
            Self::Waiting { at } => f.debug_struct("Waiting").field("at", at).finish(),
        }
    }
}

/// What the task woke for.
enum Step {
    Request(Option<Request>),
    Followed(Option<Vec<anyhow::Result<LogLine>>>),
    Event(Option<anyhow::Result<ContainerEvent>>),
    Retry,
}

struct Adapter {
    daemon: Arc<Daemon>,
    container: Container,
    settings: Settings,
    outbox: Outbox,
    /// The end of the last page that held the newest entry, until a
    /// follow starts from it.
    follow_from: Option<logs::Position>,
    follow: Follow,
    /// A `start` of the container seen while following, with no `die`
    /// after it: the ID to follow on as soon as the log stream ends.
    restarted: Option<String>,
    /// The last request was a tail that failed.
    tail_failed: bool,
    events: Option<Events>,
    retry: Option<Pin<Box<Sleep>>>,
}

/// The next item of `stream`, if there is one: in a `select!` branch
/// whose pattern is `Some(..)`, a missing stream disables the branch
/// (futures-util's `OptionFuture`,
/// <https://docs.rs/futures-util/0.3.34/futures_util/future/struct.OptionFuture.html>;
/// tokio's `select!` disables a branch whose pattern doesn't match,
/// <https://docs.rs/tokio/1.54.1/tokio/macro.select.html>).
fn next_of<S: Stream + Unpin + ?Sized>(
    stream: Option<&mut S>,
) -> OptionFuture<futures_util::stream::Next<'_, S>> {
    OptionFuture::from(stream.map(StreamExt::next))
}

impl Adapter {
    /// Requests, the follow's lines and the events until the viewer goes.
    /// A request is taken first when several are ready, so a busy follow
    /// can't hold one off.
    async fn run(mut self, mut requests: mpsc::UnboundedReceiver<Request>) {
        loop {
            let lines = match &mut self.follow {
                Follow::On { lines, .. } => Some(lines),
                _ => None,
            };
            let step = tokio::select! {
                biased;
                request = requests.recv() => Step::Request(request),
                Some(batch) = next_of(lines) => Step::Followed(batch),
                Some(event) = next_of(self.events.as_mut()) => Step::Event(event),
                Some(()) = OptionFuture::from(self.retry.as_mut()) => Step::Retry,
            };
            let open = match step {
                Step::Request(None) => false,
                Step::Request(Some(request)) => self.handle(request).await,
                Step::Followed(Some(batch)) => self.followed(batch),
                Step::Followed(None) => self.stopped(),
                Step::Event(Some(Ok(event))) => self.event(event),
                Step::Event(Some(Err(err))) => {
                    tracing::warn!(error = %format!("{err:#}"), "container events failed; retrying");
                    self.events_stopped();
                    true
                }
                Step::Event(None) => {
                    tracing::warn!("container events ended; retrying");
                    self.events_stopped();
                    true
                }
                Step::Retry => {
                    self.retry = None;
                    self.events = Some(open_events(&self.daemon));
                    self.resolve().await
                }
            };
            if !open {
                return;
            }
        }
    }

    /// `false` once the viewer is gone.
    async fn handle(&mut self, request: Request) -> bool {
        let after_failed_tail = std::mem::take(&mut self.tail_failed);
        match request {
            Request::Tail => {
                self.stop_following();
                match self.tail().await {
                    Ok(open) => open,
                    Err(err) => {
                        self.tail_failed = true;
                        self.failed(PageKind::Tail, &err)
                    }
                }
            }
            Request::Older(Position(Inner::Docker(before))) => self.older(before).await,
            Request::Newer(Position(Inner::Docker(after))) => self.newer(after).await,
            // Another source's position: none of this one's lines.
            Request::Older(_) | Request::Newer(_) => true,
            Request::Follow(true) => self.start_following(after_failed_tail).await,
            Request::Follow(false) => {
                self.stop_following();
                true
            }
        }
    }

    /// The newest entries, as a tail page. `Ok(false)` once the viewer is
    /// gone.
    async fn tail(&mut self) -> anyhow::Result<bool> {
        let started = Instant::now();
        self.follow_from = None;
        let lines = logs::tail(&self.daemon, &self.container.id, self.settings.page).await?;
        let more_older = lines.len() >= self.settings.page;
        self.follow_from = Some(lines.last().map_or(START, |l| l.position));
        Ok(self
            .outbox
            .send(page(PageKind::Tail, &lines, more_older, false, started)))
    }

    async fn older(&mut self, before: logs::Position) -> bool {
        let started = Instant::now();
        let read = logs::older(
            &self.daemon,
            &self.container.id,
            before,
            self.settings.page,
            self.container.created,
        )
        .await;
        match read {
            Ok(found) => self.outbox.send(page(
                PageKind::Older,
                &found.lines,
                found.more,
                true,
                started,
            )),
            Err(err) => self.failed(PageKind::Older, &err),
        }
    }

    async fn newer(&mut self, after: logs::Position) -> bool {
        let started = Instant::now();
        let read = logs::newer(&self.daemon, &self.container.id, after, self.settings.page).await;
        match read {
            Ok(found) => {
                if !found.more {
                    self.follow_from = Some(found.lines.last().map_or(after, |l| l.position));
                }
                self.outbox.send(page(
                    PageKind::Newer,
                    &found.lines,
                    true,
                    found.more,
                    started,
                ))
            }
            Err(err) => self.failed(PageKind::Newer, &err),
        }
    }

    /// A page that couldn't be read: an empty page that ends paging that
    /// way, and the cause as a notice (a follow's tail: the follow's).
    fn failed(&self, kind: PageKind, err: &anyhow::Error) -> bool {
        let cause = format!("{err:#}");
        tracing::warn!(direction = ?kind, error = %cause, "container log page failed");
        let empty = Event::Page {
            kind,
            lines: Vec::new(),
            oldest: None,
            newest: None,
            more_older: false,
            more_newer: false,
        };
        self.outbox.send(empty) && self.outbox.send(Event::Notice(Notice::ReadFailed(cause)))
    }

    /// Follow from the last page that held the newest entry, reading the
    /// tail first when there is none. Right after a tail that failed (the
    /// viewer asks for the tail, then the follow), the follow doesn't
    /// start, and the tail isn't read again; a later follow (`f`) reads it.
    /// A tail read for the follow that fails leaves the buffer as it is.
    async fn start_following(&mut self, after_failed_tail: bool) -> bool {
        if !matches!(self.follow, Follow::Off) {
            return true;
        }
        let cant = |cause: String| Event::Notice(Notice::CantFollow(cause));
        let from = match self.follow_from.take() {
            Some(from) => from,
            None if after_failed_tail => {
                return self.outbox.send(cant("the log couldn't be read".into()));
            }
            None => match self.tail().await {
                Ok(true) => self.follow_from.take().unwrap_or(START),
                Ok(false) => return false,
                Err(err) => {
                    let cause = format!("{err:#}");
                    tracing::warn!(error = %cause, "container log tail failed");
                    return self.outbox.send(cant(cause));
                }
            },
        };
        self.follow_at(from);
        true
    }

    fn follow_at(&mut self, at: logs::Position) {
        tracing::info!("container follow started");
        let lines =
            logs::follow(&self.daemon, &self.container.id, at).ready_chunks(self.settings.page);
        self.follow = Follow::On {
            lines: Box::pin(lines),
            at,
        };
        self.restarted = None;
    }

    fn stop_following(&mut self) {
        if !matches!(self.follow, Follow::Off) {
            tracing::info!("container follow stopped");
        }
        self.follow = Follow::Off;
        self.restarted = None;
    }

    /// A batch of the follow: its lines, then its error, which ends the
    /// follow.
    fn followed(&mut self, batch: Vec<anyhow::Result<LogLine>>) -> bool {
        let mut lines = Vec::with_capacity(batch.len());
        let mut failure = None;
        for item in batch {
            match item {
                Ok(line) => lines.push(line),
                Err(err) => failure = Some(err),
            }
        }
        if let (Some(last), Follow::On { at, .. }) = (lines.last(), &mut self.follow) {
            *at = last.position;
        }
        let mut open = true;
        if !lines.is_empty() {
            open = self
                .outbox
                .send(Event::Followed(lines.iter().map(line).collect()));
        }
        if let Some(err) = failure {
            let cause = format!("{err:#}");
            tracing::warn!(error = %cause, "container follow failed");
            self.stop_following();
            open = open && self.outbox.send(Event::Notice(Notice::CantFollow(cause)));
        }
        open
    }

    /// The daemon ended the follow: the container stopped. Follow on at
    /// once if it has started again already, else wait for it.
    fn stopped(&mut self) -> bool {
        let Follow::On { at, .. } = self.follow else {
            return true;
        };
        tracing::info!("container stopped; waiting for its start");
        self.follow = Follow::Waiting { at };
        let open = self.outbox.send(Event::Notice(Notice::Stopped));
        match self.restarted.take() {
            Some(id) => open && self.started(id),
            None => open,
        }
    }

    /// The container started again as `id`: follow on from where the
    /// follow was.
    fn started(&mut self, id: String) -> bool {
        let Follow::Waiting { at } = self.follow else {
            return true;
        };
        tracing::info!(
            recreated = id != self.container.id,
            "container started; following on"
        );
        self.container.id = id;
        self.follow_at(at);
        self.outbox.send(Event::Notice(Notice::Started))
    }

    fn event(&mut self, event: ContainerEvent) -> bool {
        if event.name != self.container.name {
            return true;
        }
        match (&event.action, &self.follow) {
            (Action::Start, Follow::Waiting { .. }) => return self.started(event.id),
            (Action::Start, Follow::On { .. }) => self.restarted = Some(event.id),
            (Action::Die, _) => self.restarted = None,
            _ => {}
        }
        true
    }

    /// While waiting, with the events stream open again: a `start` may
    /// have been missed while it was down (a dockerd restart starts its
    /// containers before it serves events), so the name is resolved
    /// again, and a running container with that name is followed on.
    async fn resolve(&mut self) -> bool {
        if !matches!(self.follow, Follow::Waiting { .. }) {
            return true;
        }
        let running = match connect::list(&self.daemon).await {
            Ok(listing) => listing
                .rows
                .into_iter()
                .find(|row| row.name == self.container.name && row.state == "running"),
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "container not resolved");
                None
            }
        };
        match running {
            Some(row) => {
                self.container.created = row.created;
                self.started(row.id)
            }
            None => true,
        }
    }

    /// The events stream ended or failed: open it again after the retry
    /// wait. Why by hand: bollard 0.21.1's `Docker::events` doesn't
    /// reconnect, futures-util has no retrying stream, and no retry crate
    /// is in the workspace's lock.
    fn events_stopped(&mut self) {
        self.events = None;
        self.retry = Some(Box::pin(tokio::time::sleep(self.settings.retry)));
    }
}

/// A page of `lines`, its edges theirs, logged with the time since
/// `started`. Nothing found: nothing more that way.
fn page(
    kind: PageKind,
    lines: &[LogLine],
    more_older: bool,
    more_newer: bool,
    started: Instant,
) -> Event {
    let more_older = more_older && !lines.is_empty();
    let more_newer = more_newer && !lines.is_empty();
    tracing::info!(
        direction = ?kind,
        count = lines.len(),
        more_older,
        more_newer,
        millis = started.elapsed().as_millis(),
        "container log page"
    );
    let edge = |l: Option<&LogLine>| l.map(|l| Position(Inner::Docker(l.position)));
    Event::Page {
        kind,
        oldest: edge(lines.first()),
        newest: edge(lines.last()),
        more_older,
        more_newer,
        lines: lines.iter().map(line).collect(),
    }
}

/// A log entry as the viewer's line: its time, the stream as its origin,
/// and its bytes without the newline Docker ends a line with.
fn line(entry: &LogLine) -> Line {
    let origin = match entry.stream {
        LogStream::Stdout => "stdout",
        LogStream::Stderr => "stderr",
        LogStream::Tty => "tty",
    };
    let mut message = entry.bytes.as_slice();
    if let Some(rest) = message.strip_suffix(b"\n") {
        message = rest.strip_suffix(b"\r").unwrap_or(rest);
    }
    Line::new(
        Timestamp::from_nanosecond(entry.position.time_ns).ok(),
        origin.as_bytes(),
        message,
        Position(Inner::Docker(entry.position)),
    )
}

/// The Containers section's list, as the picker shows it: the last list
/// read, and the footer's word on the live updates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The containers, sorted by name; `None` until a reload.
    pub rows: Option<Vec<Row>>,
    /// Why the list may be old (tui spec, "Live updates"), or `None`.
    pub trouble: Option<String>,
}

/// The Containers section kept live (tui spec, "Live updates"): bollard's
/// container events mark the list stale, and the next 4/s tick reloads it
/// with `connect::list`, so a burst costs one reload. If the events
/// stream ends or fails, the trouble is `Docker events stopped; retrying`
/// and the stream is opened again every 5 s, the list reloaded once it
/// is. Each change is published on a tokio `watch` channel
/// (<https://docs.rs/tokio/1.54.1/tokio/sync/watch/index.html>), which
/// keeps only the newest list, and the screen is woken. Dropping the
/// [`Watcher`] ends the task.
#[must_use]
pub fn watch(daemon: Arc<Daemon>, wake: Arc<Notify>) -> (Watcher, channel::Receiver<Snapshot>) {
    watch_with(daemon, wake, TICK, RETRY)
}

fn watch_with(
    daemon: Arc<Daemon>,
    wake: Arc<Notify>,
    tick: Duration,
    retry: Duration,
) -> (Watcher, channel::Receiver<Snapshot>) {
    let (tx, rx) = channel::channel(Snapshot::default());
    let task = tokio::spawn(watching(daemon, tx, wake, tick, retry));
    (Watcher(task), rx)
}

/// The Containers section's watch task; dropping it ends the task,
/// through `JoinHandle::abort` as [`SourceHandle`]. Why not tokio-util's
/// `AbortOnDropHandle`: it needs tokio-util's `rt` feature (see
/// [`SourceHandle`]'s why-not).
#[derive(Debug)]
pub struct Watcher(JoinHandle<()>);

impl Drop for Watcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn watching(
    daemon: Arc<Daemon>,
    tx: channel::Sender<Snapshot>,
    wake: Arc<Notify>,
    tick: Duration,
    retry: Duration,
) {
    let mut events = Some(open_events(&daemon));
    let mut again: Option<Pin<Box<Sleep>>> = None;
    let mut stale = false;
    let mut ticks = tokio::time::interval(tick);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let publish = |change: &dyn Fn(&mut Snapshot)| {
        tx.send_modify(|snapshot| change(snapshot));
        wake.notify_one();
    };
    loop {
        // Events first: a burst already read marks the list stale once
        // before the tick reloads it.
        tokio::select! {
            biased;
            Some(event) = next_of(events.as_mut()) => match event {
                Some(Ok(event)) => stale |= changes_rows(&event.action),
                Some(Err(err)) => {
                    tracing::warn!(error = %format!("{err:#}"), "container events failed; retrying");
                    events = None;
                }
                None => {
                    tracing::warn!("container events ended; retrying");
                    events = None;
                }
            },
            _ = ticks.tick() => {
                if stale {
                    stale = false;
                    let started = Instant::now();
                    let listed = reload(&daemon).await;
                    tracing::info!(
                        ok = listed.is_ok(),
                        millis = started.elapsed().as_millis(),
                        "containers reloaded"
                    );
                    let events_ok = events.is_some();
                    publish(&|snapshot| match &listed {
                        Ok(rows) => {
                            snapshot.rows = Some(rows.clone());
                            if events_ok {
                                snapshot.trouble = None;
                            }
                        }
                        Err(cause) => {
                            snapshot.trouble = Some(format!("couldn't refresh containers: {cause}"));
                        }
                    });
                }
            },
            Some(()) = OptionFuture::from(again.as_mut()) => {
                again = None;
                events = Some(open_events(&daemon));
                stale = true;
            },
        }
        if events.is_none() && again.is_none() {
            again = Some(Box::pin(tokio::time::sleep(retry)));
            publish(&|snapshot| {
                snapshot.trouble = Some("Docker events stopped; retrying".into());
            });
        }
        if tx.is_closed() {
            return;
        }
    }
}

/// Whether an event can change a row. A healthcheck's exec events
/// (`exec_create`, `exec_start`, `exec_die`) come on every check of
/// every container with one and change nothing the picker shows (its
/// result comes as `health_status`), so they don't reload the list.
fn changes_rows(action: &Action) -> bool {
    !matches!(action, Action::Other(other) if other.starts_with("exec_"))
}

/// The containers, sorted by name; a failed inspect leaves its container
/// out (it is in the run log, by `connect::list`).
async fn reload(daemon: &Daemon) -> Result<Vec<Row>, String> {
    connect::list(daemon)
        .await
        .map(|listing| listing.rows)
        .map_err(|err| format!("{err:#}"))
}

/// `future`'s output, for the tests' waits.
#[cfg(test)]
async fn within<F: std::future::Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("within 10 s")
}

#[cfg(test)]
mod tests;
