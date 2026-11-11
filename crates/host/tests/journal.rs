//! sb-host's journal glue through its re-export (journal spec, success
//! criterion 1): `page` at the tail, before and after a cursor, and a
//! follow woken and drained as a caller's loop does, on crafted files and
//! a current-thread runtime (`#[tokio::test]`'s default).

use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::Duration;

use sb_host::journal::{
    self, Batch, Cursor, FieldName, FollowStop, FollowWait, Follower, Journal, Limits, Page,
    PageEnd, Position, Query, Record, Release, Source, Uid, UnitName, Wake,
};
use sb_journal::testing::crafted::{
    self, A_ITEMS, D_ENTRY_ARRAY, D_N_ENTRIES, H_ARENA_SIZE, H_N_ENTRIES,
};

/// Every crafted entry carries `U=x` (`crafted::journal`).
fn every_entry() -> Query {
    Query::field(&FieldName::new(b"U").expect("a field name"), b"x")
}

fn messages(records: &[Record]) -> Vec<String> {
    records
        .iter()
        .map(|r| String::from_utf8_lossy(r.message().expect("a message")).into_owned())
        .collect()
}

fn release() -> Release {
    Release::parse("24.04").expect("supported")
}

/// A journal of five entries, `m0` to `m4`, in one crafted file.
fn five() -> (tempfile::NamedTempFile, Arc<Journal>) {
    let t = crafted::journal(5).0.file();
    let source = Source::Files(vec![t.path().to_path_buf()]);
    let journal = Journal::open(source, &release()).expect("opens");
    (t, Arc::new(journal))
}

/// The cursor of the record whose message is `m`.
fn cursor_of(page: &Page, m: &str) -> Cursor {
    let i = messages(&page.records)
        .iter()
        .position(|x| x == m)
        .expect("shown");
    page.records[i].cursor()
}

#[tokio::test]
async fn page_reads_the_newest_entries_at_the_tail() {
    let (_t, j) = five();
    let page = journal::page(j, every_entry(), Position::Tail, 2, Limits::default()).await;
    assert_eq!(messages(&page.records), ["m3", "m4"]);
    assert!(page.more_older && !page.more_newer, "{page:?}");
    assert!(page.errors.is_empty(), "{:?}", page.errors);
}

#[tokio::test]
async fn page_reads_the_entries_before_a_cursor() {
    let (_t, j) = five();
    let all = journal::page(
        j.clone(),
        every_entry(),
        Position::Tail,
        5,
        Limits::default(),
    )
    .await;
    let at = Position::Before(cursor_of(&all, "m3"));
    let page = journal::page(j, every_entry(), at, 2, Limits::default()).await;
    assert_eq!(messages(&page.records), ["m1", "m2"]);
    assert!(page.more_older && page.more_newer, "{page:?}");
}

#[tokio::test]
async fn page_reads_the_entries_after_a_cursor() {
    let (_t, j) = five();
    let all = journal::page(
        j.clone(),
        every_entry(),
        Position::Tail,
        5,
        Limits::default(),
    )
    .await;
    let at = Position::After(cursor_of(&all, "m1"));
    let page = journal::page(j, every_entry(), at, 2, Limits::default()).await;
    assert_eq!(messages(&page.records), ["m2", "m3"]);
    assert!(page.more_older && page.more_newer, "{page:?}");
}

/// One wake of a caller's follow loop, the journal spec's wake protocol
/// through sb-host's helpers ("Waiting for new entries"): `FollowWait`
/// woken within five seconds, then `drain` while `Batch::more`, and once
/// more after a batch with errors. Returns the messages, the number of
/// drains and the follower.
async fn woken(
    waiter: &mut FollowWait,
    follower: Follower,
    limits: Limits,
) -> (Vec<String>, usize, Follower) {
    let wake = tokio::time::timeout(Duration::from_secs(5), waiter.wait())
        .await
        .expect("woken within five seconds");
    assert_eq!(wake.expect("polled"), Wake::Woken);
    let mut follower = follower;
    let mut shown = Vec::new();
    let mut errors = Vec::new();
    let mut drains = 0;
    let mut retried = false;
    loop {
        let batch;
        (follower, batch) = journal::drain(follower, limits).await;
        shown.extend(messages(&batch.records));
        if !batch.errors.is_empty() {
            errors.push(format!("{:?}", batch.errors));
        }
        drains += 1;
        if batch.more {
            continue;
        }
        if !batch.errors.is_empty() && !retried {
            retried = true;
            continue;
        }
        break;
    }
    // None expected on these files: the errors' drain once more is the
    // protocol's, and every batch's errors are checked here.
    assert!(errors.is_empty(), "{errors:?}");
    (shown, drains, follower)
}

/// A follow from a page through the helpers, waited for as a caller
/// waits. Under an output limit that fits one entry per drain, the two
/// entries written since the page come over two drains; after them the
/// wait stays pending (the drains emptied the queue the wake descriptor
/// shares), the next write wakes it again, and a stop ends the follow.
#[tokio::test]
async fn a_follow_from_a_page_is_woken_drained_while_more_and_woken_again() {
    // The file grows as journald appends: one layout of six entries, of
    // which the first `n` are linked (the header's count, the shared
    // `U=x`'s count and the arrays' items), rewritten in place in one
    // write. Not truncated first: that would wake the follow before the
    // bytes are there.
    let (full, layout) = crafted::journal(6);
    let linked = |n: u64| {
        let mut c = full.clone();
        for i in n..6 {
            c.set(layout.main_array + A_ITEMS + 8 * i, 0);
            if i > 0 {
                let shared_array = c.get(layout.shared + D_ENTRY_ARRAY);
                c.set(shared_array + A_ITEMS + 8 * (i - 1), 0);
            }
        }
        c.set(H_N_ENTRIES, n);
        c.set(layout.shared + D_N_ENTRIES, n);
        c
    };
    // A directory of its own: the follow watches the file's directory, and
    // other writes to a shared one would wake it.
    let dir = tempfile::tempdir().expect("a directory");
    let path = dir.path().join("system.journal");
    let grow = |n| {
        let mut c = linked(n);
        // The arena is what follows the 272-byte header (`Crafted::new`).
        c.set(H_ARENA_SIZE, c.b.len() as u64 - 272);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .expect("opens");
        file.write_all(&c.b).expect("grow");
    };
    grow(2);
    let source = Source::Files(vec![path.clone()]);
    let j = Arc::new(Journal::open(source, &release()).expect("opens"));
    let page = journal::page(
        j.clone(),
        every_entry(),
        Position::Tail,
        10,
        Limits::default(),
    )
    .await;
    assert_eq!(messages(&page.records), ["m0", "m1"]);
    let limits = Limits::with_output(256);
    let (follower, first) = j
        .follow(every_entry(), &page.end, &limits)
        .expect("follows");
    assert!(first.records.is_empty() && !first.more, "{first:?}");
    let mut waiter = FollowWait::new(&follower).expect("a waiter");

    grow(4);
    let (shown, drains, follower) = woken(&mut waiter, follower, limits).await;
    assert_eq!(shown, ["m2", "m3"]);
    assert_eq!(drains, 2, "one entry per drain under the limit");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), waiter.wait())
            .await
            .is_err(),
        "woken after the drains"
    );

    grow(6);
    let (shown, _, _follower) = woken(&mut waiter, follower, limits).await;
    assert_eq!(shown, ["m4", "m5"]);

    waiter.stop_handle().stop();
    assert_eq!(waiter.wait().await.expect("polled"), Wake::Stopped);
}

/// What a caller moves into `spawn_blocking`, keeps across an `.await` or
/// hands to another task (journal spec, assumption 3), the helpers' futures
/// and the wake wait's included.
#[test]
fn the_re_exported_types_the_wake_fd_and_the_helpers_futures_are_send() {
    fn send<T: Send + 'static>() {}
    fn send_sync<T: Send + Sync + 'static>() {}
    fn send_value<T: Send>(_: &T) {}
    send_sync::<Journal>();
    send::<Follower>();
    send::<OwnedFd>();
    send::<Page>();
    send::<PageEnd>();
    send::<Batch>();
    send::<Record>();
    send::<Cursor>();
    send::<Query>();
    send::<Position>();
    send::<Limits>();
    send::<Source>();
    send::<Release>();
    send::<Uid>();
    send::<UnitName>();
    send::<journal::OpenError>();
    send::<journal::ReadError>();
    send::<journal::UserLookupError>();
    send::<sb_host::UbuntuReleaseError>();
    send::<FollowWait>();
    send_sync::<FollowStop>();
    send::<Wake>();
    let (_t, j) = five();
    send_value(&journal::page(
        j.clone(),
        every_entry(),
        Position::Tail,
        1,
        Limits::default(),
    ));
    let (follower, _) = j
        .follow(every_entry(), &PageEnd::default(), &Limits::default())
        .expect("follows");
    let mut waiter = FollowWait::new(&follower).expect("a waiter");
    send_value(&waiter.wait());
    send_value(&journal::drain(follower, Limits::default()));
}
