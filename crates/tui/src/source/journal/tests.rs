//! The journal adapter on crafted journal files (sb-journal's `testing`
//! builder; tui spec, Testing Strategy): tail, older and newer pages to
//! the start and the end, the cycle guard on journal Difference 20's
//! fixture, a follow that receives appended entries, a follow that can't
//! start, read errors as notices, and one unit query per source.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sb_host::journal::testing::crafted::{self, Format, H_ARENA_SIZE, H_N_ENTRIES, boot_file_in};
use sb_host::journal::{FieldName, Release, Source};
use tokio::sync::Notify;

use super::*;
use crate::source::{Generation, Tagged};

/// Every crafted entry of `crafted::journal` and the boot files carries
/// `U=x`.
fn every_entry() -> Query {
    Query::field(&FieldName::new(b"U").expect("a field name"), b"x")
}

fn release(v: &str) -> Release {
    Release::parse(v).expect("supported")
}

fn files(release: &Release, paths: &[&Path]) -> Arc<Journal> {
    let paths = paths.iter().map(|p| p.to_path_buf()).collect();
    Arc::new(Journal::open(Source::Files(paths), release).expect("opens"))
}

fn small(page: usize) -> Settings {
    Settings {
        page,
        ..Settings::default()
    }
}

/// The test's end of a source: its handle and the viewer's channel.
struct Viewer {
    handle: SourceHandle,
    events: mpsc::UnboundedReceiver<Tagged>,
    wake: Arc<Notify>,
}

impl Viewer {
    fn on(journal: Arc<Journal>, query: Query, settings: Settings) -> Self {
        let (tx, events) = mpsc::unbounded_channel();
        let wake = Arc::new(Notify::new());
        let outbox = Outbox::new(tx, Arc::clone(&wake), Generation::default().next());
        let handle = spawn_query(journal, query, settings, outbox);
        Self {
            handle,
            events,
            wake,
        }
    }

    /// The next event, within five seconds; its generation is the
    /// source's.
    async fn next(&mut self) -> Event {
        let tagged = tokio::time::timeout(Duration::from_secs(5), self.events.recv())
            .await
            .expect("an event within five seconds")
            .expect("the source is open");
        assert_eq!(tagged.generation, self.handle.generation());
        tagged.event
    }

    /// The screen was woken since the last wake taken (one permit for any
    /// number of events).
    async fn woken(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.wake.notified())
            .await
            .expect("woken");
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

    /// Followed messages until `n` have come.
    async fn followed(&mut self, n: usize) -> Vec<String> {
        messages(&self.followed_lines(n).await)
    }

    /// Followed lines until `n` have come.
    async fn followed_lines(&mut self, n: usize) -> Vec<Line> {
        let mut shown = Vec::new();
        while shown.len() < n {
            match self.next().await {
                Event::Followed(lines) => shown.extend(lines),
                other => panic!("not followed lines: {other:?}"),
            }
        }
        shown
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

fn names(range: std::ops::Range<u64>) -> Vec<String> {
    range.map(|i| format!("m{i}")).collect()
}

/// A journal of `n` entries, `m0` to `m{n-1}`, in one crafted file.
fn journal_of(n: u64) -> (tempfile::NamedTempFile, Arc<Journal>) {
    let t = crafted::journal(n).0.file();
    let j = files(&release("24.04"), &[t.path()]);
    (t, j)
}

#[tokio::test]
async fn the_tail_then_older_pages_reach_the_start_with_every_entry_once() {
    let (_t, j) = journal_of(12);
    let mut v = Viewer::on(j, every_entry(), small(5));
    let tail = v.page(Request::Tail).await;
    v.woken().await;
    assert_eq!(tail.kind, PageKind::Tail);
    assert_eq!(tail.messages, names(7..12));
    assert!(tail.more_older && !tail.more_newer, "{tail:?}");
    assert_eq!(tail.oldest.as_ref(), Some(&tail.lines[0].position));
    assert_eq!(tail.newest.as_ref(), Some(&tail.lines[4].position));
    let mut shown = tail.messages;
    let mut edge = tail.oldest.expect("an edge");
    loop {
        let page = v.page(Request::Older(edge)).await;
        assert_eq!(page.kind, PageKind::Older);
        shown.splice(0..0, page.messages);
        if !page.more_older {
            break;
        }
        edge = page.oldest.expect("an edge");
    }
    assert_eq!(shown, names(0..12));
}

#[tokio::test]
async fn newer_pages_reach_the_end_with_every_entry_once() {
    let (_t, j) = journal_of(12);
    let mut v = Viewer::on(j, every_entry(), small(5));
    let tail = v.page(Request::Tail).await;
    let older = v.page(Request::Older(tail.oldest.expect("an edge"))).await;
    let oldest = v.page(Request::Older(older.oldest.expect("an edge"))).await;
    assert_eq!(oldest.messages, ["m0", "m1"]);
    // The viewer dropped its newest end: it pages on from m1.
    let mut edge = oldest.lines[1].position.clone();
    let mut shown = Vec::new();
    loop {
        let page = v.page(Request::Newer(edge)).await;
        assert_eq!(page.kind, PageKind::Newer);
        shown.extend(page.messages);
        if !page.more_newer {
            break;
        }
        edge = page.newest.expect("an edge");
    }
    assert_eq!(shown, names(2..12));
}

/// Journal Difference 20's cycle (sb-journal's
/// `forward_paging_cycles_where_the_order_does`): on 26.04 paging forward
/// one entry at a time from b cycles c, a, b, c, ... The adapter shows c,
/// a and b once, then ends forward paging; an `Older` forgets what was
/// sent, so the same walk shows them again.
#[tokio::test]
async fn the_cycle_guard_ends_forward_paging_where_it_began() {
    let format = Format::Regular;
    let file_a = boot_file_in(format, 1, 9, &[("a", 0xa, 10, 300)], true);
    let a_tail = boot_file_in(format, 2, 9, &[("a0", 0xa, 20, 50)], false);
    let file_b = boot_file_in(format, 3, 9, &[("b", 0xb, 1, 100)], true);
    let file_c = boot_file_in(format, 4, 9, &[("c", 0xc, 1, 200)], true);
    // c's header names no tail entry (as the journal's fixture).
    let mut bytes = std::fs::read(file_c.path()).expect("read");
    bytes[264..272].fill(0);
    std::fs::write(file_c.path(), &bytes).expect("write");
    let j = files(
        &release("26.04"),
        &[file_a.path(), a_tail.path(), file_b.path(), file_c.path()],
    );
    let who = FieldName::new(b"WHO").expect("a field name");
    let all = j.page(
        &every_entry(),
        &journal::Position::Tail,
        4,
        &Limits::default(),
    );
    let b = all
        .records
        .iter()
        .find(|r| r.field(&who) == Some(b"b"))
        .expect("b")
        .cursor();
    let mut v = Viewer::on(Arc::clone(&j), every_entry(), small(1));

    // Forward from each page's newest edge for as long as pages bring
    // lines, whatever `more_newer` says, as the journal's own test pages.
    let walk = async |v: &mut Viewer| {
        let mut shown = Vec::new();
        let mut edge = position(b);
        for _ in 0..8 {
            let page = v.page(Request::Newer(edge.clone())).await;
            if page.lines.is_empty() {
                assert!(!page.more_newer, "{page:?}");
                return shown;
            }
            for line in &page.lines {
                shown.push(line.position.clone());
            }
            edge = page.newest.expect("an edge");
        }
        panic!("forward paging didn't end: {shown:?}");
    };
    let first = walk(&mut v).await;
    // Each entry is its boot's only one: its cursor's `b=` names it.
    let who_of = |p: &Position| {
        let Position(Inner::Journal(c)) = p else {
            panic!("a journal position: {p:?}");
        };
        let text = c.to_string();
        let boot = text
            .split(';')
            .find_map(|f| f.strip_prefix("b="))
            .expect("a boot");
        match &boot[..2] {
            "0a" => "a",
            "0b" => "b",
            "0c" => "c",
            other => panic!("boot {other}"),
        }
    };
    assert_eq!(
        first.iter().map(who_of).collect::<Vec<_>>(),
        ["c", "a", "b"]
    );

    // An older page forgets what was sent: the same walk shows them again.
    v.page(Request::Older(position(b))).await;
    let again = walk(&mut v).await;
    assert_eq!(
        again.iter().map(who_of).collect::<Vec<_>>(),
        ["c", "a", "b"]
    );
}

/// A `Tail` forgets what newer pages sent: the viewer's buffer starts
/// again, so the same newer page shows its entries again.
#[tokio::test]
async fn a_tail_forgets_what_newer_pages_sent() {
    let (_t, j) = journal_of(6);
    let mut v = Viewer::on(j, every_entry(), small(2));
    let tail = v.page(Request::Tail).await;
    let older = v.page(Request::Older(tail.oldest.expect("an edge"))).await;
    let oldest = v.page(Request::Older(older.oldest.expect("an edge"))).await;
    let m1 = oldest.lines[1].position.clone();
    assert_eq!(
        v.page(Request::Newer(m1.clone())).await.messages,
        ["m2", "m3"]
    );
    assert_eq!(v.page(Request::Tail).await.messages, ["m4", "m5"]);
    assert_eq!(v.page(Request::Newer(m1)).await.messages, ["m2", "m3"]);
}

#[test]
fn the_guard_remembers_the_newest_sent_up_to_its_cap() {
    let cursor = |n: u64| {
        let Position(Inner::Journal(c)) = crate::testing::position(n) else {
            panic!("a journal position");
        };
        c
    };
    let mut sent = Sent::new(2);
    sent.remember(cursor(1));
    sent.remember(cursor(2));
    sent.remember(cursor(2));
    assert!(sent.holds(&cursor(1)) && sent.holds(&cursor(2)));
    sent.remember(cursor(3));
    assert!(!sent.holds(&cursor(1)), "the oldest sent goes first");
    assert!(sent.holds(&cursor(2)) && sent.holds(&cursor(3)));
    // The viewer's edge below the newest sent: what was sent after it goes.
    sent.remember(cursor(4));
    assert!(sent.forget_after(&cursor(3)));
    assert!(sent.holds(&cursor(3)) && !sent.holds(&cursor(4)));
    assert!(
        !sent.forget_after(&cursor(3)),
        "the newest sent: nothing to forget"
    );
    assert!(!sent.forget_after(&cursor(9)), "never sent");
    assert!(sent.holds(&cursor(3)));
    sent.clear();
    assert!(!sent.holds(&cursor(3)));
    let mut none = Sent::new(0);
    none.remember(cursor(1));
    assert!(!none.holds(&cursor(1)));
}

/// A file that grows as journald appends: `crafted::journal(6)`'s layout,
/// of which the first `n` entries are linked, rewritten in place (not
/// truncated first, which would wake the follow before the bytes are
/// there), in a directory of its own, as sb-host's follow test does.
struct Growing {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Growing {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let path = dir.path().join("system.journal");
        Self { _dir: dir, path }
    }

    fn grow(&self, n: u64) {
        Self::write(&self.path, n, n);
    }

    /// `n` entries counted, the first `linked` of them linked: the rest
    /// are slots journald hasn't written yet, which the follow waits on.
    fn write(path: &Path, n: u64, linked: u64) {
        let (mut c, layout) = crafted::journal(6);
        for i in linked..6 {
            c.set_array_item(layout.main_array, i, 0);
            if i > 0 {
                let shared_array = c.get(layout.shared + crafted::D_ENTRY_ARRAY);
                c.set_array_item(shared_array, i - 1, 0);
            }
        }
        c.set(H_N_ENTRIES, n);
        c.set(layout.shared + crafted::D_N_ENTRIES, n);
        c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .expect("opens");
        file.write_all(&c.b).expect("grow");
    }

    fn journal(&self) -> Arc<Journal> {
        files(&release("24.04"), &[&self.path])
    }
}

#[tokio::test]
async fn a_follow_from_the_tail_receives_appended_entries_once() {
    let file = Growing::new();
    file.grow(2);
    // One entry per drain: the wake protocol drains on while `more`.
    let settings = Settings {
        limits: Limits::with_output(256),
        ..small(10)
    };
    let mut v = Viewer::on(file.journal(), every_entry(), settings);
    // A page holds one entry under the limit too.
    assert_eq!(v.page(Request::Tail).await.messages, ["m1"]);
    v.handle.send(Request::Follow(true));
    // Following twice starts one follow.
    v.handle.send(Request::Follow(true));
    v.quiet().await;
    file.grow(4);
    assert_eq!(v.followed(2).await, ["m2", "m3"]);
    v.woken().await;
    file.grow(6);
    assert_eq!(v.followed(2).await, ["m4", "m5"]);
    v.quiet().await;
}

/// The handover: entries written between the tail and the follow's start
/// come in the follow's first batch, drained on while the output limit
/// cuts it, each once.
#[tokio::test]
async fn entries_written_before_the_follow_starts_come_first_once() {
    let file = Growing::new();
    file.grow(2);
    let settings = Settings {
        limits: Limits::with_output(256),
        ..small(10)
    };
    let mut v = Viewer::on(file.journal(), every_entry(), settings);
    assert_eq!(v.page(Request::Tail).await.messages, ["m1"]);
    file.grow(5);
    v.handle.send(Request::Follow(true));
    assert_eq!(v.followed(3).await, ["m2", "m3", "m4"]);
    v.quiet().await;
    file.grow(6);
    assert_eq!(v.followed(1).await, ["m5"]);
}

/// follow-L1: a read the follow waited on (an entry slot journald hadn't
/// written yet) is read again on the source's tick, not only at the
/// journal's next write. The slot is written through a hard link in
/// another directory, which the follow's watch on the file's own
/// directory doesn't see (inotify reports a directory's entries by the
/// path they were opened through, inotify(7)), so only the tick can show
/// it.
#[tokio::test]
async fn a_waited_read_is_retried_on_the_tick() {
    let file = Growing::new();
    file.grow(2);
    // A slow tick, so the slot is written well before it comes.
    let settings = Settings {
        retry: Duration::from_secs(2),
        ..small(10)
    };
    let mut v = Viewer::on(file.journal(), every_entry(), settings);
    assert_eq!(v.page(Request::Tail).await.messages, ["m0", "m1"]);
    v.handle.send(Request::Follow(true));
    v.quiet().await;
    // m2 counted, its slot not written: the follow's drain waits on it.
    Growing::write(&file.path, 3, 2);
    v.quiet().await;
    let elsewhere = tempfile::tempdir().expect("a directory");
    let link = elsewhere.path().join("system.journal");
    std::fs::hard_link(&file.path, &link).expect("linked");
    Growing::write(&link, 3, 3);
    assert_eq!(v.followed(1).await, ["m2"]);
}

/// Another kind of source's position names none of the journal's
/// entries: nothing is read, and the source goes on.
#[tokio::test]
async fn a_container_position_is_ignored() {
    let file = Growing::new();
    file.grow(2);
    let mut v = Viewer::on(file.journal(), every_entry(), small(10));
    let docker = Position(Inner::Docker(sb_docker::logs::Position {
        time_ns: 0,
        ordinal: 0,
    }));
    v.handle.send(Request::Older(docker.clone()));
    v.handle.send(Request::Newer(docker));
    v.quiet().await;
    assert_eq!(v.page(Request::Tail).await.messages, ["m0", "m1"]);
}

/// A unit with no entries yet (newly installed, or vacuumed): an empty
/// tail, then a follow from its end gets the first entries.
#[tokio::test]
async fn a_unit_with_no_entries_yet_is_followed_from_its_empty_tail() {
    let file = Growing::new();
    file.grow(0);
    let mut v = Viewer::on(file.journal(), every_entry(), small(10));
    let tail = v.page(Request::Tail).await;
    assert!(tail.lines.is_empty() && tail.oldest.is_none() && tail.newest.is_none());
    assert!(!tail.more_older && !tail.more_newer, "{tail:?}");
    v.handle.send(Request::Follow(true));
    v.quiet().await;
    file.grow(2);
    assert_eq!(v.followed(2).await, ["m0", "m1"]);
}

/// The viewer dropped its newest end while following and pages on from
/// its new edge: what was sent past it isn't taken for a cycle.
#[tokio::test]
async fn newer_pages_after_the_viewer_dropped_followed_lines_show_them() {
    let file = Growing::new();
    file.grow(2);
    let mut v = Viewer::on(file.journal(), every_entry(), small(10));
    let tail = v.page(Request::Tail).await;
    v.handle.send(Request::Follow(true));
    v.quiet().await;
    file.grow(4);
    assert_eq!(v.followed(2).await, ["m2", "m3"]);
    v.handle.send(Request::Follow(false));
    let newer = v.page(Request::Newer(tail.newest.expect("an edge"))).await;
    assert_eq!(newer.messages, ["m2", "m3"]);
    assert!(!newer.more_newer);
}

#[tokio::test]
async fn following_with_no_newest_page_reads_the_tail_first() {
    let file = Growing::new();
    file.grow(3);
    let mut v = Viewer::on(file.journal(), every_entry(), small(10));
    let tail = v.page(Request::Follow(true)).await;
    assert_eq!(tail.kind, PageKind::Tail);
    assert_eq!(tail.messages, ["m0", "m1", "m2"]);
    file.grow(4);
    assert_eq!(v.followed(1).await, ["m3"]);
}

/// A stopped follow is freed; the next starts from the end of the
/// newest page since, here a forward page with nothing newer, so nothing
/// is shown twice.
#[tokio::test]
async fn a_follow_stopped_and_restarted_from_a_newer_page_repeats_nothing() {
    let file = Growing::new();
    file.grow(2);
    let mut v = Viewer::on(file.journal(), every_entry(), small(10));
    v.page(Request::Tail).await;
    v.handle.send(Request::Follow(true));
    v.quiet().await;
    file.grow(3);
    let m2 = v.followed_lines(1).await.remove(0);
    assert_eq!(m2.message, b"m2");
    v.handle.send(Request::Follow(false));
    v.quiet().await;
    file.grow(4);
    v.quiet().await;
    // From m2, the viewer's newest line.
    let newer = v.page(Request::Newer(m2.position)).await;
    assert_eq!(newer.messages, ["m3"]);
    assert!(!newer.more_newer);
    v.handle.send(Request::Follow(true));
    v.quiet().await;
    file.grow(5);
    assert_eq!(v.followed(1).await, ["m4"]);
}

/// A follow that can't start is a notice; the view stays paged, and `f`
/// retries from the same page's end.
#[tokio::test]
async fn a_follow_that_cant_start_is_a_notice_and_f_retries() {
    let dir = tempfile::tempdir().expect("a directory");
    let journals = dir.path().join("journal");
    std::fs::create_dir(&journals).expect("created");
    let path = journals.join("system.journal");
    let crafted = crafted::journal(2).0.file();
    std::fs::copy(crafted.path(), &path).expect("copied");
    let source = Source::Directory(journals.clone());
    let j = Arc::new(Journal::open(source, &release("24.04")).expect("opens"));
    let mut v = Viewer::on(j, every_entry(), small(10));
    assert_eq!(v.page(Request::Tail).await.messages, ["m0", "m1"]);
    // Nothing left to watch.
    std::fs::remove_dir_all(&journals).expect("removed");
    match v.ask(Request::Follow(true)).await {
        Event::Notice(notice @ Notice::CantFollow(_)) => {
            assert!(notice.to_string().ends_with("; f retries"), "{notice}");
        }
        other => panic!("not the notice: {other:?}"),
    }
    std::fs::create_dir(&journals).expect("created again");
    v.handle.send(Request::Follow(true));
    // No tail read again: the retry starts from the kept end.
    v.quiet().await;
}

/// Read errors: the first as a notice with the count of the others.
#[tokio::test]
async fn read_errors_are_a_notice_after_the_page() {
    let t = crafted::journal(2).0.file();
    let dir = tempfile::tempdir().expect("a directory");
    let gone = [dir.path().join("a.journal"), dir.path().join("b.journal")];
    let paths: Vec<&Path> = [t.path(), &gone[0], &gone[1]].into();
    let mut v = Viewer::on(files(&release("24.04"), &paths), every_entry(), small(10));
    assert_eq!(v.page(Request::Tail).await.messages, ["m0", "m1"]);
    match v.next().await {
        Event::Notice(Notice::ReadErrors { first, others }) => {
            assert!(first.contains("a.journal"), "{first}");
            assert_eq!(others, 1);
        }
        other => panic!("not the notice: {other:?}"),
    }
}

/// `spawn` pages `Journal::unit_query`'s matches: the unit's own entries,
/// not another unit's, with journalctl's origin.
#[tokio::test]
async fn a_service_source_pages_its_units_entries_with_their_origin() {
    let entries: &[&[&[u8]]] = &[
        &[
            b"_SYSTEMD_UNIT=app.service",
            b"SYSLOG_IDENTIFIER=app",
            b"SYSLOG_PID=7",
            b"_PID=8",
        ],
        &[b"_SYSTEMD_UNIT=other.service", b"SYSLOG_IDENTIFIER=other"],
        &[b"_SYSTEMD_UNIT=app.service", b"_COMM=app-bin", b"_PID=9"],
        &[
            b"_SYSTEMD_UNIT=app.service",
            b"_COMM=app-bin",
            b"SYSLOG_IDENTIFIER=id",
        ],
        &[b"_SYSTEMD_UNIT=app.service"],
    ];
    let t = crafted::journal_of(entries).0.file();
    let j = files(&release("24.04"), &[t.path()]);
    let (tx, mut events) = mpsc::unbounded_channel();
    let outbox = Outbox::new(tx, Arc::new(Notify::new()), Generation::default());
    let unit = UnitName::new("app").expect("a unit");
    let handle = spawn(j, &unit, None, outbox);
    handle.send(Request::Tail);
    let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("in time")
        .expect("an event")
        .event;
    let Event::Page { lines, .. } = event else {
        panic!("not a page: {event:?}");
    };
    assert_eq!(messages(&lines), ["m0", "m2", "m3", "m4"]);
    let origins: Vec<&[u8]> = lines.iter().map(|l| l.origin.as_slice()).collect();
    assert_eq!(origins, [b"app[7]".as_slice(), b"app-bin[9]", b"id", b""]);
    assert!(lines.iter().all(|l| l.time.is_some()));
}

/// The viewer gone: the source ends at its next event.
#[tokio::test]
async fn a_source_whose_viewer_is_gone_ends() {
    let (_t, j) = journal_of(2);
    let v = Viewer::on(j, every_entry(), small(5));
    let Viewer { handle, events, .. } = v;
    drop(events);
    handle.send(Request::Tail);
    // The task returns rather than waiting for more requests: the handle's
    // channel closes from the task's side.
    tokio::time::timeout(Duration::from_secs(5), handle.requests.closed())
        .await
        .expect("the source ended");
}

/// follow-L4: a follow the system's inotify limits stop says which limit,
/// with the system's own error; any other cause is its error chain.
#[test]
fn an_inotify_limit_is_named_in_the_follows_notice() {
    let errno = std::io::Error::from;
    let instances = OpenError::Inotify {
        source: errno(rustix::io::Errno::MFILE),
    };
    assert_eq!(
        cant_follow(&instances),
        format!(
            "create the follow's inotify instance: {}; the system's inotify limit \
             (fs.inotify.max_user_instances) may be reached",
            errno(rustix::io::Errno::MFILE)
        )
    );
    let watches = OpenError::Watch {
        path: PathBuf::from("/j"),
        source: errno(rustix::io::Errno::NOSPC),
    };
    assert_eq!(
        cant_follow(&watches),
        format!(
            "watch the journal directory /j: {}; the system's inotify limit \
             (fs.inotify.max_user_watches) may be reached",
            errno(rustix::io::Errno::NOSPC)
        )
    );
    // Another errno is no limit: the chain alone.
    let nomem = OpenError::Inotify {
        source: errno(rustix::io::Errno::NOMEM),
    };
    assert_eq!(
        cant_follow(&nomem),
        format!(
            "create the follow's inotify instance: {}",
            errno(rustix::io::Errno::NOMEM)
        )
    );
    let denied = OpenError::Watch {
        path: PathBuf::from("/j"),
        source: errno(rustix::io::Errno::ACCESS),
    };
    assert_eq!(
        cant_follow(&denied),
        format!(
            "watch the journal directory /j: {}",
            errno(rustix::io::Errno::ACCESS)
        )
    );
    assert_eq!(
        cant_follow(&OpenError::NothingToWatch),
        OpenError::NothingToWatch.to_string()
    );
}

/// Every entry's message by its position: what a search's matches name.
fn by_position(j: &Journal, n: usize) -> std::collections::HashMap<Position, String> {
    let page = j.page(
        &every_entry(),
        &journal::Position::Tail,
        n,
        &Limits::default(),
    );
    page.records
        .iter()
        .map(|r| {
            let line = line(r);
            let message = String::from_utf8_lossy(&line.message).into_owned();
            (line.position, message)
        })
        .collect()
}

/// The viewer's stand-in match: one in a message ending `00`.
fn hundreds() -> Matcher {
    Matcher::new(|line: &Line| usize::from(line.message.ends_with(b"00")))
}

fn search_settings(step: usize, matches: usize) -> Settings {
    Settings {
        search_step: step,
        matches,
        ..Settings::default()
    }
}

impl Viewer {
    /// One side's `Found` events for `search` until it ends: the matches'
    /// messages in the order sent, the steps, and how it ended.
    async fn found(
        &mut self,
        names: &std::collections::HashMap<Position, String>,
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
                    shown.extend(found.iter().map(|f| {
                        assert_eq!(f.count, 1);
                        names[&f.position].clone()
                    }));
                    if let Some(end) = end {
                        return (shown, steps, order, end);
                    }
                }
                other => panic!("not this side's step: {other:?}"),
            }
        }
    }
}

/// A search reads past the tail: the older side's matches come nearest
/// the buffer first, a step at a time, to the start of the log.
#[tokio::test]
async fn a_search_finds_the_older_matches_nearest_first() {
    let (_t, j) = journal_of(3_000);
    let by = by_position(&j, 3_000);
    let mut v = Viewer::on(Arc::clone(&j), every_entry(), search_settings(400, 10_000));
    let tail = v.page(Request::Tail).await;
    assert_eq!(tail.messages, names(2_500..3_000));
    v.handle.send(Request::Search(Search {
        id: 7,
        matcher: hundreds(),
        older: tail.oldest.clone(),
        newer: None,
    }));
    let (found, steps, order, end) = v.found(&by, 7, Side::Older).await;
    let expected: Vec<String> = (1..25).rev().map(|i| format!("m{i}00")).collect();
    assert_eq!(found, expected);
    assert_eq!(order, Order::NearestFirst);
    assert_eq!(end, SearchEnd::Done);
    // 2,500 entries in steps of 400, then the end.
    assert_eq!(steps, 7 + 1);
}

/// A jump's page: the entry, 250 before it and 249 after, a new buffer
/// with more on both sides; then the newer side's matches come farthest
/// first, to the end of the log.
#[tokio::test]
async fn around_a_match_then_the_newer_side_farthest_first() {
    let (_t, j) = journal_of(3_000);
    let by = by_position(&j, 3_000);
    let at = by
        .iter()
        .find(|(_, m)| m.as_str() == "m1000")
        .map(|(p, _)| p.clone())
        .expect("m1000");
    let mut v = Viewer::on(Arc::clone(&j), every_entry(), search_settings(400, 10_000));
    let around = v.page(Request::Around(at)).await;
    assert_eq!(around.kind, PageKind::Around);
    assert_eq!(around.messages, names(750..1_250));
    assert!(around.more_older && around.more_newer, "{around:?}");
    v.handle.send(Request::Search(Search {
        id: 8,
        matcher: hundreds(),
        older: None,
        newer: around.newest.clone(),
    }));
    let (found, _, order, end) = v.found(&by, 8, Side::Newer).await;
    let expected: Vec<String> = (13..30).rev().map(|i| format!("m{i}00")).collect();
    assert_eq!(found, expected);
    assert_eq!(order, Order::FarthestFirst);
    assert_eq!(end, SearchEnd::Done);
}

/// The older side stops at its cap, the nearest kept.
#[tokio::test]
async fn the_older_side_stops_at_its_cap() {
    let (_t, j) = journal_of(3_000);
    let by = by_position(&j, 3_000);
    let mut v = Viewer::on(Arc::clone(&j), every_entry(), search_settings(400, 5));
    let tail = v.page(Request::Tail).await;
    v.handle.send(Request::Search(Search {
        id: 9,
        matcher: hundreds(),
        older: tail.oldest.clone(),
        newer: None,
    }));
    let (found, _, _, end) = v.found(&by, 9, Side::Older).await;
    assert_eq!(found, ["m2400", "m2300", "m2200", "m2100", "m2000"]);
    assert_eq!(end, SearchEnd::Capped);
    v.quiet().await;
}

/// A stopped search sends no more and never ends as done: its scan stops
/// at its next step.
#[tokio::test]
async fn a_stopped_search_goes_quiet() {
    let (_t, j) = journal_of(3_000);
    let mut v = Viewer::on(Arc::clone(&j), every_entry(), search_settings(1, 10_000));
    let tail = v.page(Request::Tail).await;
    let slow = Matcher::new(|_: &Line| {
        std::thread::sleep(Duration::from_millis(2));
        0
    });
    v.handle.send(Request::Search(Search {
        id: 10,
        matcher: slow,
        older: tail.oldest.clone(),
        newer: None,
    }));
    assert!(matches!(v.next().await, Event::Found { search: 10, .. }));
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

/// A search side's read errors don't end it: they are one notice, the
/// first with the count of the others, before its end (as a page's are).
#[tokio::test]
async fn a_search_sides_read_errors_are_one_notice_before_its_end() {
    let t = crafted::journal(100).0.file();
    let dir = tempfile::tempdir().expect("a directory");
    let gone = [dir.path().join("a.journal"), dir.path().join("b.journal")];
    let paths: Vec<&Path> = [t.path(), &gone[0], &gone[1]].into();
    let settings = Settings {
        search_step: 10,
        ..small(10)
    };
    let mut v = Viewer::on(files(&release("24.04"), &paths), every_entry(), settings);
    let tail = v.page(Request::Tail).await;
    assert!(matches!(
        v.next().await,
        Event::Notice(Notice::ReadErrors { .. })
    ));
    v.handle.send(Request::Search(Search {
        id: 11,
        matcher: hundreds(),
        older: tail.oldest.clone(),
        newer: None,
    }));
    let mut notices = Vec::new();
    loop {
        match v.next().await {
            Event::Found { end: None, .. } => {}
            Event::Found { end: Some(end), .. } => {
                assert_eq!(end, SearchEnd::Done);
                break;
            }
            Event::Notice(notice) => notices.push(notice),
            other => panic!("{other:?}"),
        }
    }
    let [Notice::ReadErrors { first, others }] = &notices[..] else {
        panic!("one notice: {notices:?}")
    };
    assert!(first.contains("a.journal"), "{first}");
    assert_eq!(*others, 1);
}

/// Home's page: the log's first entries, a new buffer, not following.
#[tokio::test]
async fn a_head_page_is_the_journals_first_entries() {
    let (_t, j) = journal_of(1_000);
    let mut v = Viewer::on(Arc::clone(&j), every_entry(), small(5));
    let _ = v.page(Request::Tail).await;
    let head = v.page(Request::Head).await;
    assert_eq!(head.kind, PageKind::Head);
    assert_eq!(head.messages, names(0..5));
    assert!(!head.more_older && head.more_newer, "{head:?}");
    let on = v.page(Request::Newer(head.newest.expect("an edge"))).await;
    assert_eq!(on.messages, names(5..10));
}
