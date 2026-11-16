//! What is made ahead: asked for, made off the viewer's thread, kept
//! within its bound, dropped when no longer wanted, texts used only for
//! the window, format and search they were made for; the worker ends
//! with its viewer.

use jiff::tz::TimeZone;
use sb_cli_core::theme::{Palette, Profile, Theme};

use super::*;
use crate::testing;

fn format() -> Format {
    Format::new(
        TimeZone::UTC,
        Theme::resolve(&Palette::default(), Profile::TrueColor),
    )
}

/// Long lines, numbered by their ids.
fn lines() -> HashMap<u64, Line> {
    (0..10)
        .map(|id| {
            let message = format!("m{id} {}", "lorem ipsum ".repeat(420));
            (id, testing::line(id, &message))
        })
        .collect()
}

/// The window from column 4,000, 240 wide, as the viewer asks for it.
fn window(f: &Format, query: &str) -> Window {
    Window {
        format: f.clone(),
        query: query.to_owned(),
        left: 3970,
        reach: 4300,
        column: 4000,
        width: 240,
    }
}

#[test]
fn lines_asked_for_come_back_with_their_places() {
    let (f, lines) = (format(), lines());
    let mut ahead = Ahead::new(Arc::new(Notify::new()));
    ahead.want(&[(1, true), (2, true), (3, true)], &f, None, |id| {
        lines.get(&id)
    });
    assert_eq!(ahead.pending(), 3);
    ahead.settle();
    assert_eq!(ahead.pending(), 0);
    assert!(ahead.bytes() > 0);
    let (places, text) = ahead.take(1, &f, "");
    assert!(places.expect("line 1's places").is_whole());
    assert!(text.is_none(), "no window: places only");
    assert!(ahead.take(1, &f, "").0.is_some(), "places stay kept");
    // Asked again while kept or pending: no second job.
    ahead.want(&[(1, true), (2, true), (3, true)], &f, None, |_| {
        panic!("asked again")
    });
    // No longer wanted: dropped.
    ahead.want(&[(3, true)], &f, None, |id| lines.get(&id));
    assert!(ahead.take(1, &f, "").0.is_none());
    assert!(ahead.take(2, &f, "").0.is_none());
    assert!(ahead.take(3, &f, "").0.is_some());
    ahead.want(&[], &f, None, |id| lines.get(&id));
    assert_eq!(ahead.bytes(), 0);
}

/// With a window, each line's text is made for it, and used only while
/// the window, the format and the search are the ones it was made for;
/// its places are kept either way.
#[test]
fn texts_are_made_for_the_window_and_used_only_for_it() {
    let (f, lines) = (format(), lines());
    let mut ahead = Ahead::new(Arc::new(Notify::new()));
    ahead.want(&[(1, true), (2, true)], &f, Some(window(&f, "")), |id| {
        lines.get(&id)
    });
    ahead.settle();
    let (places, text) = ahead.take(1, &f, "");
    let far = text.expect("line 1's text");
    assert!(places.is_some());
    assert!(far.from <= 3970 && far.columns >= 4300, "{far:?}");
    // Another search: the text isn't for it.
    let (places, text) = ahead.take(2, &f, "ipsum");
    assert!(places.is_some() && text.is_none());
    // A new window: the kept places are sent again for its text, and the
    // old text isn't used.
    ahead.want(&[(1, true), (2, true)], &f, Some(window(&f, "")), |id| {
        lines.get(&id)
    });
    assert_eq!(ahead.pending(), 2, "taken, so asked again");
    ahead.settle();
    let mut moved = window(&f, "");
    moved.left = 1970;
    moved.reach = 2300;
    moved.column = 2000;
    ahead.want(&[(1, true), (2, true)], &f, Some(moved), |id| {
        lines.get(&id)
    });
    assert_eq!(ahead.pending(), 2, "texts for the new window");
    ahead.settle();
    let far = ahead.take(1, &f, "").1.expect("the new window's text");
    assert!(far.from <= 1970 && far.columns >= 2300, "{far:?}");
    // A theme or `t` change keeps the places; another colour drops them.
    let mut t = f.clone();
    t.meta = false;
    ahead.want(&[(2, true)], &t, None, |id| lines.get(&id));
    assert!(ahead.bytes() > 0);
    let ascii = Format::new(
        TimeZone::UTC,
        Theme::resolve(&Palette::default(), Profile::Ascii),
    );
    ahead.want(&[(2, true)], &ascii, None, |_| None);
    assert_eq!(ahead.bytes(), 0);
}

#[test]
fn whats_made_stays_within_its_bound_and_isnt_asked_for_again() {
    let (f, lines) = (format(), lines());
    let mut ahead = Ahead::new(Arc::new(Notify::new())).with_limit(1);
    ahead.want(&[(1, true)], &f, None, |id| lines.get(&id));
    ahead.settle();
    assert_eq!(ahead.bytes(), 0);
    assert!(ahead.take(1, &f, "").0.is_none());
    ahead.want(&[(1, true)], &f, None, |_| {
        panic!("refused, so not asked again")
    });
    // Once no longer wanted, it may be asked for again later.
    ahead.want(&[], &f, None, |id| lines.get(&id));
    ahead.want(&[(1, true)], &f, None, |id| lines.get(&id));
    assert_eq!(ahead.pending(), 1);
}

#[test]
fn the_worker_wakes_the_screen_and_ends_with_its_viewer() {
    let (f, lines) = (format(), lines());
    let wake = Arc::new(Notify::new());
    let mut ahead = Ahead::new(Arc::clone(&wake));
    let wanted = Arc::clone(&ahead.wanted);
    ahead.want(&[(1, true)], &f, None, |id| lines.get(&id));
    let woken = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime")
        .block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(60), wake.notified()).await
        });
    assert!(woken.is_ok(), "the worker woke the screen");
    drop(ahead);
    let started = std::time::Instant::now();
    while Arc::strong_count(&wanted) > 1 {
        assert!(started.elapsed().as_secs() < 60, "the worker ended");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn nothing_is_asked_for_without_a_wake() {
    let (f, lines) = (format(), lines());
    let mut ahead = Ahead::default();
    ahead.want(&[(1, true)], &f, None, |id| lines.get(&id));
    assert_eq!(ahead.pending(), 0);
    ahead.settle();
}

/// A late result of an earlier ask (another search, since cleared) never
/// takes the place of a text that serves the window now.
#[test]
fn a_late_result_keeps_the_text_that_serves_now() {
    let (f, lines) = (format(), lines());
    let mut ahead = Ahead::new(Arc::new(Notify::new()));
    ahead.want(&[(1, true)], &f, Some(window(&f, "")), |id| lines.get(&id));
    ahead.settle();
    // Line 1 was asked for again for a search since cleared: its result
    // comes back while the window wants no search again.
    let search = Arc::new(window(&f, "lorem"));
    ahead.pending.insert(1, Some(Arc::clone(&search)));
    let made = make(Job {
        id: 1,
        line: lines[&1].clone(),
        format: Arc::new(f.clone()),
        resumes: None,
        window: Some(Arc::clone(&search)),
    });
    assert!(made.text.is_some());
    ahead.keep(Done {
        id: 1,
        window: Some(search),
        made,
        fresh: true,
    });
    assert_eq!(ahead.pending(), 0);
    let text = ahead.take(1, &f, "").1;
    assert!(text.is_some(), "the text for no search, kept");
}

/// The line id whose making panics on the worker (`make`).
pub(super) const PANICS: u64 = u64::MAX;

/// A panic on the worker isn't swallowed: the viewer resumes it, so the
/// screen ends as it would had the parse panicked on its own thread.
#[test]
fn a_panic_on_the_worker_reaches_the_viewer() {
    let f = format();
    let line = testing::line(PANICS, "boom");
    let mut ahead = Ahead::new(Arc::new(Notify::new()));
    ahead.want(&[(PANICS, true)], &f, None, |_| Some(&line));
    let resumed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ahead.settle()));
    let panic = resumed.expect_err("the worker's panic, resumed");
    let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(
        message.contains("a test's panic on the worker"),
        "{message}"
    );
}

/// Lines already made aren't parsed from their start again: a line
/// taken for a frame keeps its places, so asking for its text again
/// starts from them.
#[test]
fn a_taken_lines_places_stay_kept() {
    let (f, lines) = (format(), lines());
    let mut ahead = Ahead::new(Arc::new(Notify::new()));
    ahead.want(&[(1, true), (2, false)], &f, Some(window(&f, "")), |id| {
        lines.get(&id)
    });
    ahead.settle();
    assert_eq!(ahead.fresh(), 2);
    let (places, text) = ahead.take(1, &f, "");
    assert!(places.is_some() && text.is_some());
    assert!(ahead.take(2, &f, "").0.is_some(), "places only, still kept");
    let mut moved = window(&f, "");
    moved.left = 1970;
    moved.reach = 2300;
    moved.column = 2000;
    ahead.want(&[(1, true), (2, false)], &f, Some(moved), |id| {
        lines.get(&id)
    });
    assert_eq!(ahead.pending(), 1, "line 1's text for the new window");
    ahead.settle();
    assert_eq!(ahead.fresh(), 2, "from the places kept");
    assert!(ahead.take(1, &f, "").1.is_some());
}

/// At most [`ASK_AT_ONCE`] lines are asked for at once; the rest when
/// asked again, as the worker's results wake the viewer.
#[test]
fn a_few_lines_are_asked_for_at_once() {
    let f = format();
    let many: HashMap<u64, Line> = (0..100)
        .map(|id| (id, testing::line(id, &"lorem ipsum ".repeat(50))))
        .collect();
    let ids: Vec<(u64, bool)> = (0..100).map(|id| (id, false)).collect();
    let mut ahead = Ahead::new(Arc::new(Notify::new()));
    ahead.want(&ids, &f, None, |id| many.get(&id));
    assert_eq!(ahead.pending(), ASK_AT_ONCE);
    assert!(ahead.wants_more());
    ahead.settle();
    ahead.want(&ids, &f, None, |id| many.get(&id));
    assert_eq!(ahead.pending(), ASK_AT_ONCE);
    while ahead.wants_more() {
        ahead.settle();
        ahead.want(&ids, &f, None, |id| many.get(&id));
    }
    ahead.settle();
    assert_eq!(ahead.kept(), 100);
}

/// Each line done is taken in at the viewer's next wake, and a text
/// handed out leaves only its places in the bytes kept.
#[test]
fn whats_done_is_taken_in_and_counted() {
    let (f, lines) = (format(), lines());
    let wake = Arc::new(Notify::new());
    let mut ahead = Ahead::new(Arc::clone(&wake));
    ahead.want(&[(1, true)], &f, Some(window(&f, "")), |id| lines.get(&id));
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a runtime")
        .block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(60), wake.notified()).await
        })
        .expect("woken");
    ahead.drain();
    assert_eq!(ahead.pending(), 0);
    let with_text = ahead.bytes();
    let text = ahead.take(1, &f, "").1.expect("its text");
    assert_eq!(ahead.bytes(), with_text - text_bytes(&text.text));
}
