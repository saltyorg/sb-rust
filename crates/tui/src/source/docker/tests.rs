//! The docker adapter on the fake Engine API (sb-docker's `testing`; tui
//! spec, Testing Strategy): the tail, older pages back to the start and
//! newer pages to the end with every entry once (entries sharing one
//! timestamp included), a follow, the stream ending on a stop and going
//! on after a `start`, a re-created container with a new ID, errors as
//! notices; and the Containers section's live list.

use std::collections::HashMap;
use std::time::Duration;

use sb_docker::connect::connect;
use sb_docker::testing::{FakeEngine, FakeEntry, FakeLog, Options, container_event};
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;
use crate::source::{Generation, Tagged};

const ID: &str = "c0ffee0000000000000000000000000000000000000000000000000000000001";
const NEW_ID: &str = "c0ffee0000000000000000000000000000000000000000000000000000000002";
const NAME: &str = "app";

/// 2026-11-13 10:00:00 UTC, the log's first second.
fn base() -> Timestamp {
    "2026-11-13T10:00:00Z".parse().expect("a time")
}

/// Entry `i` of a log: four a second, and every tenth shares the
/// previous one's time (Docker's nanoseconds can repeat, the platform
/// note); stderr every seventh.
fn entry(i: u32) -> FakeEntry {
    let quarter = i - i / 10;
    let time = base() + jiff::SignedDuration::from_millis(i64::from(quarter) * 250);
    let entry = FakeEntry::new(time, format!("line {i}\n"));
    if i.is_multiple_of(7) {
        entry.on(LogStream::Stderr)
    } else {
        entry
    }
}

fn log_of(n: u32) -> FakeLog {
    FakeLog {
        tty: false,
        entries: (0..n).map(entry).collect(),
    }
}

/// A container's inspect answer.
fn inspect(id: &str, name: &str, status: &str) -> Value {
    json!({ "Id": id, "Name": format!("/{name}"), "State": { "Status": status } })
}

fn engine(logs: HashMap<String, FakeLog>) -> FakeEngine {
    FakeEngine::start(Options {
        inspects: vec![inspect(ID, NAME, "running")],
        logs,
        ..Options::default()
    })
    .expect("the fake Engine API")
}

fn container() -> Container {
    Container {
        id: ID.into(),
        name: NAME.into(),
        // A second before the log: older pages stop there.
        created: Some(base() - jiff::SignedDuration::from_secs(1)),
    }
}

fn small(page: usize) -> Settings {
    Settings {
        page,
        retry: Duration::from_millis(100),
        ..Settings::default()
    }
}

/// The test's end of a source: its handle and the viewer's channel.
struct Viewer {
    handle: SourceHandle,
    events: mpsc::UnboundedReceiver<Tagged>,
}

impl Viewer {
    async fn on(engine: &FakeEngine, container: Container, settings: Settings) -> Self {
        let daemon = Arc::new(connect(&engine.host()).await.expect("connects"));
        let (tx, events) = mpsc::unbounded_channel();
        let outbox = Outbox::new(tx, Arc::new(Notify::new()), Generation::default());
        let handle = spawn_with(daemon, container, settings, outbox);
        Self { handle, events }
    }

    async fn next(&mut self) -> Event {
        within(self.events.recv())
            .await
            .expect("the source is open")
            .event
    }

    /// No event within 300 ms.
    async fn quiet(&mut self) {
        let got = tokio::time::timeout(Duration::from_millis(300), self.events.recv()).await;
        assert!(got.is_err(), "{got:?}");
    }

    async fn ask(&mut self, request: Request) -> Event {
        self.handle.send(request);
        self.next().await
    }

    /// The page answering `request`: its messages, edges and flags.
    async fn page(&mut self, request: Request) -> Paged {
        match self.ask(request).await {
            Event::Page {
                kind,
                lines,
                oldest,
                newest,
                more_older,
                more_newer,
            } => Paged {
                kind,
                messages: messages(&lines),
                lines,
                oldest,
                newest,
                more_older,
                more_newer,
            },
            other => panic!("not a page: {other:?}"),
        }
    }

    /// The next event's lines, which must be a follow's.
    async fn followed(&mut self) -> Vec<String> {
        match self.next().await {
            Event::Followed(lines) => messages(&lines),
            other => panic!("not followed lines: {other:?}"),
        }
    }

    async fn notice(&mut self) -> Notice {
        match self.next().await {
            Event::Notice(notice) => notice,
            other => panic!("not a notice: {other:?}"),
        }
    }
}

#[derive(Debug)]
struct Paged {
    kind: PageKind,
    messages: Vec<String>,
    lines: Vec<Line>,
    oldest: Option<Position>,
    newest: Option<Position>,
    more_older: bool,
    more_newer: bool,
}

fn messages(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|l| String::from_utf8_lossy(&l.message).into_owned())
        .collect()
}

fn all(range: std::ops::Range<u32>) -> Vec<String> {
    range.map(|i| format!("line {i}")).collect()
}

/// The tail, then older pages back to the creation: every entry once, in
/// order, entries sharing a timestamp across page edges included.
#[tokio::test]
async fn older_pages_reach_the_start_with_every_entry_once() {
    let engine = engine(HashMap::from([(ID.into(), log_of(230))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    let tail = v.page(Request::Tail).await;
    assert_eq!(tail.kind, PageKind::Tail);
    assert_eq!(tail.messages, all(180..230));
    assert!(tail.more_older && !tail.more_newer);
    let mut shown = tail.messages;
    let mut edge = tail.oldest.expect("an edge");
    loop {
        let page = v.page(Request::Older(edge.clone())).await;
        assert_eq!(page.kind, PageKind::Older);
        shown.splice(0..0, page.messages);
        if !page.more_older {
            break;
        }
        edge = page.oldest.expect("an edge");
    }
    assert_eq!(shown, all(0..230));
}

/// Newer pages from the first entry to the end, every entry once; the
/// last page's end is where a follow starts.
#[tokio::test]
async fn newer_pages_reach_the_end_and_the_follow_goes_on_from_there() {
    let engine = engine(HashMap::from([(ID.into(), log_of(130))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    let first = v.page(Request::Tail).await;
    // Back to the start, then forward again from its first line.
    let mut edge = first.oldest.expect("an edge");
    let start = loop {
        let page = v.page(Request::Older(edge.clone())).await;
        if !page.more_older {
            break page;
        }
        edge = page.oldest.expect("an edge");
    };
    assert_eq!(start.messages[0], "line 0");
    let mut shown = vec![start.messages[0].clone()];
    let mut edge = start.oldest.expect("the first line");
    loop {
        let page = v.page(Request::Newer(edge.clone())).await;
        assert_eq!(page.kind, PageKind::Newer);
        shown.extend(page.messages);
        if !page.more_newer {
            break;
        }
        edge = page.newest.expect("an edge");
    }
    assert_eq!(shown, all(0..130));
    // Following from the end: only what comes after.
    v.handle.send(Request::Follow(true));
    engine.append(ID, entry(130));
    assert_eq!(v.followed().await, ["line 130"]);
}

/// An empty newer page at the end has no edge and no more.
#[tokio::test]
async fn a_newer_page_at_the_end_is_empty() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    let tail = v.page(Request::Tail).await;
    assert!(!tail.more_older, "a short log has nothing older");
    let page = v.page(Request::Newer(tail.newest.expect("edge"))).await;
    assert_eq!(page.messages, Vec::<String>::new());
    assert_eq!((page.newest, page.more_newer), (None, false));
}

/// Follow reads the tail first when no page holds the newest entry,
/// then sends appended lines; a second `Follow(true)` changes nothing;
/// `Follow(false)` stops it.
#[tokio::test]
async fn a_follow_sends_appended_lines_until_stopped() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    let tail = v.page(Request::Follow(true)).await;
    assert_eq!(tail.kind, PageKind::Tail);
    assert_eq!(tail.messages, all(0..3));
    v.handle.send(Request::Follow(true));
    engine.append(ID, entry(3));
    assert_eq!(v.followed().await, ["line 3"]);
    v.handle.send(Request::Follow(false));
    // The stop is taken before the next append is read.
    v.quiet().await;
    engine.append(ID, entry(4));
    v.quiet().await;
}

/// An empty log's follow sends the first lines written.
#[tokio::test]
async fn an_empty_logs_follow_sends_everything() {
    let engine = engine(HashMap::from([(ID.into(), FakeLog::default())]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    let tail = v.page(Request::Tail).await;
    assert_eq!(tail.messages, Vec::<String>::new());
    assert_eq!((tail.oldest, tail.more_older), (None, false));
    v.handle.send(Request::Follow(true));
    engine.append(ID, entry(0));
    assert_eq!(v.followed().await, ["line 0"]);
}

/// The container stops: the follow says so and waits; a `start` event
/// for its name follows on from where it was.
#[tokio::test]
async fn a_follow_goes_on_across_a_restart() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    v.page(Request::Tail).await;
    v.handle.send(Request::Follow(true));
    engine.append(ID, entry(3));
    assert_eq!(v.followed().await, ["line 3"]);
    engine.end_follows(ID);
    assert_eq!(v.notice().await, Notice::Stopped);
    // Another container's start, and a die, change nothing.
    engine.event(container_event("start", "other", "other"));
    engine.event(container_event("die", ID, NAME));
    v.quiet().await;
    engine.resume_follows(ID);
    engine.append(ID, entry(4));
    engine.event(container_event("start", ID, NAME));
    assert_eq!(v.notice().await, Notice::Started);
    assert_eq!(v.followed().await, ["line 4"]);
}

/// A re-created container has a new ID: the follow goes on in its log.
#[tokio::test]
async fn a_follow_goes_on_in_a_recreated_container() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    v.page(Request::Follow(true)).await;
    engine.end_follows(ID);
    assert_eq!(v.notice().await, Notice::Stopped);
    engine.append(NEW_ID, entry(3));
    engine.event(container_event("start", NEW_ID, NAME));
    assert_eq!(v.notice().await, Notice::Started);
    assert_eq!(v.followed().await, ["line 3"]);
    // Its older pages are the new container's.
    let tail = v.page(Request::Tail).await;
    assert_eq!(tail.messages, ["line 3"]);
}

/// A `start` seen while still following (a restart quicker than the end
/// of the log stream) resumes the follow as soon as the stream ends,
/// with no event after it; a `die` after the `start` cancels that.
#[tokio::test]
async fn a_start_seen_while_following_resumes_at_the_streams_end() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    v.page(Request::Follow(true)).await;
    engine.event(container_event("start", ID, NAME));
    // Taken before the stream's end below.
    v.quiet().await;
    engine.end_follows(ID);
    assert_eq!(v.notice().await, Notice::Stopped);
    assert_eq!(v.notice().await, Notice::Started);
    // The new follow ends at once (the fake's follows stay ended): no
    // start since, so it waits.
    assert_eq!(v.notice().await, Notice::Stopped);
    v.quiet().await;
    engine.resume_follows(ID);
    engine.event(container_event("start", ID, NAME));
    engine.event(container_event("die", ID, NAME));
    assert_eq!(v.notice().await, Notice::Started);
    // The die came after the start: the next end waits.
    v.quiet().await;
    engine.end_follows(ID);
    assert_eq!(v.notice().await, Notice::Stopped);
    v.quiet().await;
}

/// A start missed while the events stream was down (a dockerd restart):
/// once the stream is open again, the name is resolved, and a running
/// container is followed on; one still stopped is waited for.
#[tokio::test]
async fn a_start_missed_while_events_were_down_is_found_by_name() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    v.page(Request::Follow(true)).await;
    engine.set_inspects(vec![inspect(ID, NAME, "exited")]);
    engine.end_follows(ID);
    assert_eq!(v.notice().await, Notice::Stopped);
    // The stream ends, and each reopened one at once: the name is
    // resolved after every retry, and the container is still stopped.
    engine.end_events();
    tokio::time::sleep(Duration::from_millis(350)).await;
    v.quiet().await;
    // It runs again, re-created, with no event seen.
    engine.set_inspects(vec![inspect(NEW_ID, NAME, "running")]);
    engine.append(NEW_ID, entry(3));
    assert_eq!(v.notice().await, Notice::Started);
    assert_eq!(v.followed().await, ["line 3"]);
}

/// A container the daemon doesn't know: the tail is an empty page and the
/// cause; the follow asked right after says it can't, without reading
/// again; a later follow reads the tail again and says why it can't.
#[tokio::test]
async fn a_failed_read_is_a_notice_and_paging_ends() {
    let engine = engine(HashMap::new());
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    let tail = v.page(Request::Tail).await;
    assert_eq!(tail.kind, PageKind::Tail);
    assert!(tail.messages.is_empty() && !tail.more_older && !tail.more_newer);
    let Notice::ReadFailed(cause) = v.notice().await else {
        panic!("not a failed read");
    };
    assert!(cause.contains("No such container"), "{cause}");
    let reads = |e: &FakeEngine| e.paths().iter().filter(|p| p.contains("/logs")).count();
    assert_eq!(reads(&engine), 1);
    assert_eq!(
        v.ask(Request::Follow(true)).await,
        Event::Notice(Notice::CantFollow("the log couldn't be read".into()))
    );
    assert_eq!(reads(&engine), 1, "not read again");
    let Event::Notice(Notice::CantFollow(cause)) = v.ask(Request::Follow(true)).await else {
        panic!("not a failed follow");
    };
    assert!(cause.contains("No such container"), "{cause}");
    assert_eq!(reads(&engine), 2);
    // Older and newer pages too: empty, nothing more, the cause.
    let at = Position(Inner::Docker(logs::Position {
        time_ns: base().as_nanosecond(),
        ordinal: 0,
    }));
    for (request, kind) in [
        (Request::Older(at.clone()), PageKind::Older),
        (Request::Newer(at), PageKind::Newer),
    ] {
        let page = v.page(request).await;
        assert_eq!(page.kind, kind);
        assert!(page.messages.is_empty() && !page.more_older && !page.more_newer);
        assert!(matches!(v.notice().await, Notice::ReadFailed(_)));
    }
}

/// Another kind of source's position names none of this log's lines.
#[tokio::test]
async fn a_journal_position_is_ignored() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    v.handle.send(Request::Older(crate::testing::position(1)));
    v.handle.send(Request::Newer(crate::testing::position(1)));
    v.quiet().await;
    assert!(!engine.paths().iter().any(|p| p.contains("/logs")));
}

/// A stopped events stream is opened again after the retry.
#[tokio::test]
async fn the_events_stream_is_opened_again() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let _v = Viewer::on(&engine, container(), small(50)).await;
    engine.end_events();
    let opened = |e: &FakeEngine| e.paths().iter().filter(|p| p.contains("/events")).count();
    within(async {
        while opened(&engine) < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A follow's batch: its lines first, then its error, which ends the
/// follow with the cause.
#[tokio::test]
async fn a_failing_follow_says_why_after_its_lines() {
    let engine = engine(HashMap::from([(ID.into(), log_of(3))]));
    let daemon = Arc::new(connect(&engine.host()).await.expect("connects"));
    let (tx, mut events) = mpsc::unbounded_channel();
    let outbox = Outbox::new(tx, Arc::new(Notify::new()), Generation::default());
    let mut adapter = Adapter {
        daemon,
        container: container(),
        settings: small(50),
        outbox,
        follow_from: None,
        follow: Follow::Off,
        restarted: None,
        tail_failed: false,
        events: None,
        retry: None,
        search: None,
    };
    adapter.follow_at(START);
    let line = LogLine {
        position: logs::Position {
            time_ns: base().as_nanosecond(),
            ordinal: 0,
        },
        stream: LogStream::Stdout,
        bytes: b"last words\n".to_vec(),
    };
    assert!(adapter.followed(vec![Ok(line), Err(anyhow::anyhow!("broken pipe"))]));
    assert!(
        matches!(adapter.follow, Follow::Off),
        "{:?}",
        adapter.follow
    );
    let got: Vec<Event> = std::iter::from_fn(|| events.try_recv().ok().map(|t| t.event)).collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(matches!(&got[0], Event::Followed(lines) if messages(lines) == ["last words"]));
    assert_eq!(
        got[1],
        Event::Notice(Notice::CantFollow("broken pipe".into()))
    );
    // A stream's end while not following, or a start while not waiting,
    // changes nothing.
    assert!(adapter.stopped());
    assert!(adapter.started(NEW_ID.into()));
    assert_eq!(adapter.container.id, ID);
    assert!(events.try_recv().is_err());
}

/// The stream is the origin; the newline a line ends with (and a carriage
/// return before it) is left out, a part of a long line kept whole.
#[test]
fn a_log_entry_is_a_line() {
    let at = logs::Position {
        time_ns: base().as_nanosecond(),
        ordinal: 2,
    };
    let of = |stream, bytes: &[u8]| {
        line(&LogLine {
            position: at,
            stream,
            bytes: bytes.to_vec(),
        })
    };
    let out = of(LogStream::Stdout, b"hello\n");
    assert_eq!(
        (out.origin.as_slice(), out.message.as_slice()),
        (&b"stdout"[..], &b"hello"[..])
    );
    assert_eq!(out.time, Some(base()));
    assert_eq!(out.position, Position(Inner::Docker(at)));
    let err = of(LogStream::Stderr, b"oops\r\n");
    assert_eq!(
        (err.origin.as_slice(), err.message.as_slice()),
        (&b"stderr"[..], &b"oops"[..])
    );
    let tty = of(LogStream::Tty, b"prompt\r\n");
    assert_eq!(
        (tty.origin.as_slice(), tty.message.as_slice()),
        (&b"tty"[..], &b"prompt"[..])
    );
    let part = of(LogStream::Stdout, b"a part\r");
    assert_eq!(part.message, b"a part\r");
}

/// The Containers section's list: an event reloads it (a burst, once),
/// the list's failure and the events' end are its trouble.
#[tokio::test]
async fn the_watch_reloads_on_events_and_reports_trouble() {
    let engine = engine(HashMap::new());
    let daemon = Arc::new(connect(&engine.host()).await.expect("connects"));
    let wake = Arc::new(Notify::new());
    engine.set_inspects(vec![
        inspect(ID, NAME, "running"),
        inspect(NEW_ID, "web", "exited"),
    ]);
    // Given before the stream opens, the fake sends them at once.
    for _ in 0..5 {
        engine.event(container_event("start", ID, NAME));
    }
    let (watcher, mut rx) = watch_with(
        daemon,
        Arc::clone(&wake),
        Duration::from_millis(50),
        Duration::from_millis(100),
    );
    within(rx.changed()).await.expect("a change");
    within(wake.notified()).await;
    let names = |rx: &mut channel::Receiver<Snapshot>| {
        rx.borrow_and_update()
            .rows
            .as_ref()
            .map(|rows| rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>())
    };
    assert_eq!(names(&mut rx), Some(vec!["app".into(), "web".into()]));
    assert_eq!(rx.borrow().trouble, None);
    let lists = |e: &FakeEngine| {
        e.paths()
            .iter()
            .filter(|p| p.contains("/containers/json"))
            .count()
    };
    // The burst cost one reload.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(lists(&engine), 1);
    engine.end_events();
    within(async {
        loop {
            rx.changed().await.expect("open");
            if rx.borrow_and_update().trouble.as_deref() == Some("Docker events stopped; retrying")
            {
                break;
            }
        }
    })
    .await;
    drop(watcher);
}

/// A healthcheck's exec events, which Docker sends on every check, change
/// no row: they don't reload the list (`health_status` does).
#[tokio::test]
async fn exec_events_dont_reload_the_list() {
    let engine = engine(HashMap::new());
    for action in [
        "exec_create: /bin/sh -c true",
        "exec_start: /bin/sh -c true",
        "exec_die",
    ] {
        engine.event(container_event(action, ID, NAME));
    }
    let daemon = Arc::new(connect(&engine.host()).await.expect("connects"));
    let (_watcher, _rx) = watch_with(
        daemon,
        Arc::new(Notify::new()),
        Duration::from_millis(20),
        Duration::from_millis(100),
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let lists = engine
        .paths()
        .iter()
        .filter(|p| p.contains("/containers/json"))
        .count();
    assert_eq!(lists, 0);
    engine.event(container_event("health_status: unhealthy", ID, NAME));
    within(async {
        while !engine
            .paths()
            .iter()
            .any(|p| p.contains("/containers/json"))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A reload that fails keeps the last list and says why.
#[tokio::test]
async fn a_failed_reload_keeps_the_list_and_says_why() {
    let engine = FakeEngine::start(Options {
        list: Some(json!("not a list")),
        ..Options::default()
    })
    .expect("the fake Engine API");
    let daemon = Arc::new(connect(&engine.host()).await.expect("connects"));
    let (_watcher, mut rx) = watch_with(
        daemon,
        Arc::new(Notify::new()),
        Duration::from_millis(50),
        Duration::from_millis(100),
    );
    engine.event(container_event("start", ID, NAME));
    within(rx.changed()).await.expect("a change");
    let snapshot = rx.borrow_and_update().clone();
    assert_eq!(snapshot.rows, None);
    let trouble = snapshot.trouble.expect("trouble");
    assert!(
        trouble.starts_with("couldn't refresh containers: "),
        "{trouble}"
    );
}

/// The watch ends when the picker is gone.
#[tokio::test]
async fn the_watch_ends_with_its_receiver() {
    let engine = engine(HashMap::new());
    let daemon = Arc::new(connect(&engine.host()).await.expect("connects"));
    let (tx, rx) = channel::channel(Snapshot::default());
    let task = tokio::spawn(watching(
        daemon,
        tx,
        Arc::new(Notify::new()),
        Duration::from_millis(20),
        Duration::from_millis(50),
    ));
    drop(rx);
    engine.event(container_event("start", ID, NAME));
    within(task).await.expect("ended");
}

/// The viewer's stand-in match: one in a line whose number ends `0`.
fn tens() -> Matcher {
    Matcher::new(|line: &Line| usize::from(line.message.ends_with(b"0")))
}

impl Viewer {
    /// One side's `Found` events for `search` until it ends: the matches'
    /// messages, the steps, the order and how it ended. A match's entry
    /// is read by its position from `by`.
    async fn found(
        &mut self,
        by: &HashMap<Position, String>,
        search: u64,
        side: Side,
    ) -> (Vec<String>, usize, Order, SearchEnd) {
        let (mut shown, mut steps) = (Vec::new(), 0);
        loop {
            match self.next().await {
                Event::Found {
                    search: id,
                    side: s,
                    order,
                    found,
                    end,
                    ..
                } if id == search && s == side => {
                    steps += 1;
                    shown.extend(found.iter().map(|f| by[&f.position].clone()));
                    if let Some(end) = end {
                        return (shown, steps, order, end);
                    }
                }
                other => panic!("not this side's step: {other:?}"),
            }
        }
    }
}

/// Every line's message by its position, from pages back to the start.
async fn by_position(v: &mut Viewer) -> HashMap<Position, String> {
    let tail = v.page(Request::Tail).await;
    let mut lines = tail.lines;
    let mut edge = tail.oldest;
    while let Some(at) = edge {
        let older = v.page(Request::Older(at)).await;
        edge = older.more_older.then_some(older.oldest).flatten();
        lines.extend(older.lines);
    }
    lines
        .into_iter()
        .map(|l| {
            let message = String::from_utf8_lossy(&l.message).into_owned();
            (l.position, message)
        })
        .collect()
}

/// A container's search: the older side reads from the creation, its
/// matches farthest first, in steps of log text, to the buffer.
#[tokio::test]
async fn a_search_reads_the_older_side_from_the_creation() {
    let engine = engine(HashMap::from([(ID.into(), log_of(300))]));
    let settings = Settings {
        search_step: 200,
        ..small(50)
    };
    let mut v = Viewer::on(&engine, container(), settings).await;
    let by = by_position(&mut v).await;
    let tail = v.page(Request::Tail).await;
    assert_eq!(tail.messages[0], "line 250");
    v.handle.send(Request::Search(Search {
        id: 3,
        matcher: tens(),
        older: tail.oldest.clone(),
        newer: None,
    }));
    let (found, steps, order, end) = v.found(&by, 3, Side::Older).await;
    let expected: Vec<String> = (0..25).map(|i| format!("line {}", i * 10)).collect();
    assert_eq!(found, expected);
    assert_eq!(order, Order::FarthestFirst);
    assert_eq!(end, SearchEnd::Done);
    assert!(steps > 2, "{steps}");
}

/// A jump's page: the entry with the page before it and the rest of a
/// page after, not following; then the newer side's matches nearest
/// first, stopping at the cap.
#[tokio::test]
async fn around_a_match_then_the_newer_side_to_its_cap() {
    let engine = engine(HashMap::from([(ID.into(), log_of(300))]));
    let settings = Settings {
        matches: 4,
        ..small(50)
    };
    let mut v = Viewer::on(&engine, container(), settings).await;
    let by = by_position(&mut v).await;
    let at = by
        .iter()
        .find(|(_, m)| m.as_str() == "line 100")
        .map(|(p, _)| p.clone())
        .expect("line 100");
    let around = v.page(Request::Around(at)).await;
    assert_eq!(around.kind, PageKind::Around);
    let expected: Vec<String> = (75..125).map(|i| format!("line {i}")).collect();
    assert_eq!(around.messages, expected);
    assert!(around.more_older && around.more_newer, "{around:?}");
    v.handle.send(Request::Search(Search {
        id: 4,
        matcher: tens(),
        older: None,
        newer: around.newest.clone(),
    }));
    let (found, _, order, end) = v.found(&by, 4, Side::Newer).await;
    assert_eq!(found, ["line 130", "line 140", "line 150", "line 160"]);
    assert_eq!(order, Order::NearestFirst);
    assert_eq!(end, SearchEnd::Capped);
}

/// A log that can't be read: a search's side ends failed with the
/// daemon's cause.
#[tokio::test]
async fn a_log_that_cant_be_read_fails_the_search() {
    // No log for the container: the daemon's 404.
    let engine = engine(HashMap::new());
    let mut v = Viewer::on(&engine, container(), small(50)).await;
    let at = Position(Inner::Docker(logs::Position {
        time_ns: base().as_nanosecond(),
        ordinal: 0,
    }));
    v.handle.send(Request::Search(Search {
        id: 5,
        matcher: tens(),
        older: Some(at),
        newer: None,
    }));
    loop {
        match v.next().await {
            Event::Found { end: Some(end), .. } => {
                assert!(matches!(end, SearchEnd::Failed(_)), "{end:?}");
                break;
            }
            Event::Found { .. } => {}
            other => panic!("{other:?}"),
        }
    }
}

/// Home's page: the container log's first lines, a new buffer; a read
/// that fails is a notice only.
#[tokio::test]
async fn a_head_page_is_the_containers_first_lines() {
    let fake = engine(HashMap::from([(ID.into(), log_of(300))]));
    let mut v = Viewer::on(&fake, container(), small(50)).await;
    let head = v.page(Request::Head).await;
    assert_eq!(head.kind, PageKind::Head);
    let expected: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
    assert_eq!(head.messages, expected);
    assert!(!head.more_older && head.more_newer, "{head:?}");

    let empty = engine(HashMap::new());
    let mut v = Viewer::on(&empty, container(), small(50)).await;
    match v.ask(Request::Head).await {
        Event::Notice(Notice::ReadFailed(_)) => {}
        other => panic!("{other:?}"),
    }
}

/// Home on a log that fits one page reaches its end: the follow goes on
/// from its last line, sending only what comes after, without reading
/// the tail again.
#[tokio::test]
async fn a_short_logs_head_page_is_followed_from_its_last_line() {
    let fake = engine(HashMap::from([(ID.into(), log_of(3))]));
    let mut v = Viewer::on(&fake, container(), small(50)).await;
    let head = v.page(Request::Head).await;
    assert_eq!(head.messages, all(0..3));
    assert!(!head.more_newer, "{head:?}");
    v.handle.send(Request::Follow(true));
    fake.append(ID, entry(3));
    assert_eq!(v.followed().await, ["line 3"]);
}

/// A jump near the end: the page around the entry reaches the log's end,
/// and the follow goes on from its last line, sending only what comes
/// after.
#[tokio::test]
async fn a_page_around_an_entry_near_the_end_is_followed_from_its_last_line() {
    let fake = engine(HashMap::from([(ID.into(), log_of(60))]));
    let mut v = Viewer::on(&fake, container(), small(50)).await;
    let by = by_position(&mut v).await;
    let at = by
        .iter()
        .find(|(_, m)| m.as_str() == "line 55")
        .map(|(p, _)| p.clone())
        .expect("line 55");
    let around = v.page(Request::Around(at)).await;
    assert_eq!(around.kind, PageKind::Around);
    assert_eq!(around.messages, all(30..60));
    assert!(around.more_older && !around.more_newer, "{around:?}");
    v.handle.send(Request::Follow(true));
    fake.append(ID, entry(60));
    assert_eq!(v.followed().await, ["line 60"]);
}

/// A stopped search goes quiet: steps already on their way may come, but
/// no end, and nothing once the side has seen the stop. The entries are
/// 16 KiB, so the side's read yields to the source every 16 of them
/// (sb-docker's `scan`), and the stop is taken then.
#[tokio::test]
async fn a_stopped_search_goes_quiet() {
    let wide = |i: u32| {
        let time = base() + jiff::SignedDuration::from_millis(i64::from(i) * 250);
        FakeEntry::new(time, format!("line {i} {}\n", "x".repeat(16 * 1024)))
    };
    let log = FakeLog {
        tty: false,
        entries: (0..300).map(wide).collect(),
    };
    let fake = engine(HashMap::from([(ID.into(), log)]));
    let settings = Settings {
        search_step: 1,
        ..small(50)
    };
    let mut v = Viewer::on(&fake, container(), settings).await;
    let tail = v.page(Request::Tail).await;
    v.handle.send(Request::Search(Search {
        id: 6,
        matcher: Matcher::new(|_: &Line| 0),
        older: tail.oldest.clone(),
        newer: None,
    }));
    assert!(matches!(v.next().await, Event::Found { search: 6, .. }));
    v.handle.send(Request::StopSearch);
    loop {
        let got = tokio::time::timeout(Duration::from_millis(300), v.events.recv()).await;
        match got {
            Err(_) => break,
            Ok(Some(Tagged {
                event: Event::Found { end, .. },
                ..
            })) => assert_eq!(end, None),
            Ok(other) => panic!("{other:?}"),
        }
    }
}

/// A stale list waits for the ticks' next time on their grid, as it did
/// while they ran all along: one at the time itself is that time.
#[tokio::test(start_paused = true)]
async fn the_next_tick_is_on_the_grid() {
    let start = tokio::time::Instant::now();
    let tick = Duration::from_millis(250);
    let at = |ms: u64| start + Duration::from_millis(ms);
    assert_eq!(next_tick(start, tick, start), start);
    assert_eq!(next_tick(start, tick, at(1)), at(250));
    assert_eq!(next_tick(start, tick, at(250)), at(250));
    assert_eq!(next_tick(start, tick, at(251)), at(500));
    assert_eq!(next_tick(start, tick, at(3_600_010)), at(3_600_250));
}

/// The gate: no tick waited for while the list is fresh; a stale list
/// waits for the grid's next tick; a reload that outlasts a tick leaves
/// that tick due, waited for even with the list fresh, and a list gone
/// stale then takes it, not the grid's next.
#[tokio::test(start_paused = true)]
async fn the_reload_tick_is_waited_for_only_while_wanted() {
    let start = tokio::time::Instant::now();
    let tick = Duration::from_millis(250);
    let at = |ms: u64| start + Duration::from_millis(ms);
    let mut gate = TickGate::new(start, tick);
    assert!(!gate.waits(false));
    assert!(gate.waits(true));
    assert_eq!(gate.stale_at(at(10)), Some(at(250)));
    gate.ticked();
    // A reload of 100 ms: within its tick.
    gate.reloaded(at(250), at(350));
    assert!(!gate.waits(false));
    assert_eq!(gate.stale_at(at(400)), Some(at(500)));
    // A reload past the next tick's time: that tick is due.
    gate.ticked();
    gate.reloaded(at(500), at(750));
    assert!(gate.waits(false));
    assert_eq!(gate.stale_at(at(760)), None);
    gate.ticked();
    assert!(!gate.waits(false));
    assert_eq!(gate.stale_at(at(800)), Some(at(1_000)));
}
