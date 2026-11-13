//! The viewer against the fake source (tui spec, Testing Strategy):
//! scrolling, paging requests near both edges, follow paused by
//! scrolling and resumed at the bottom, the buffer's caps by entries and
//! by bytes, notices, events of an old generation dropped, and
//! `TestBackend` snapshots at 120×40 and 80×24.

use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use jiff::tz::TimeZone;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sb_cli_core::theme::{Palette, Profile, Theme};

use super::*;
use crate::testing::{FakeSource, fake_source, line, position};

struct Harness {
    viewer: Viewer,
    fake: FakeSource,
    terminal: Terminal<TestBackend>,
}

fn format() -> Format {
    Format::new(
        TimeZone::UTC,
        Theme::resolve(&Palette::default(), Profile::Ascii),
    )
}

fn open(width: u16, height: u16, limits: Limits) -> Harness {
    open_with(width, height, limits, format())
}

fn open_with(width: u16, height: u16, limits: Limits, format: Format) -> Harness {
    let mut fake = None;
    let viewer = Viewer::with_limits(
        "app",
        Kind::Service,
        format,
        limits,
        Arc::new(Notify::new()),
        |outbox| {
            let (handle, source) = fake_source(outbox);
            fake = Some(source);
            handle
        },
    );
    let terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    Harness {
        viewer,
        fake: fake.expect("opened"),
        terminal,
    }
}

impl Harness {
    /// The source sends `event`, the screen wakes, and a frame is drawn.
    fn send(&mut self, event: Event) -> Flow {
        assert!(self.fake.send(event));
        let flow = self.viewer.wake();
        self.draw();
        flow
    }

    fn draw(&mut self) {
        let viewer = &mut self.viewer;
        self.terminal.draw(|f| viewer.draw(f)).expect("drawn");
    }

    fn key(&mut self, code: KeyCode) -> Flow {
        let flow = self
            .viewer
            .input(Input::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        self.draw();
        flow
    }

    fn requests(&mut self) -> Vec<Request> {
        std::iter::from_fn(|| self.fake.request()).collect()
    }

    /// The screen's rows, trailing spaces trimmed.
    fn screen(&self) -> Vec<String> {
        let buffer = self.terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// The log rows (the screen without its header), blank ones dropped.
    fn log(&self) -> Vec<String> {
        self.screen()
            .into_iter()
            .skip(1)
            .filter(|row| !row.is_empty())
            .collect()
    }
}

fn lines(range: std::ops::Range<u64>) -> Vec<Line> {
    range.map(|n| line(n, &format!("m{n}"))).collect()
}

fn tail(range: std::ops::Range<u64>, more_older: bool) -> Event {
    let ls = lines(range.clone());
    Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older,
        more_newer: false,
    }
}

fn older(range: std::ops::Range<u64>, more_older: bool) -> Event {
    let ls = lines(range);
    Event::Page {
        kind: PageKind::Older,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older,
        more_newer: true,
    }
}

fn newer(range: std::ops::Range<u64>, more_newer: bool) -> Event {
    let ls = lines(range);
    Event::Page {
        kind: PageKind::Newer,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: true,
        more_newer,
    }
}

fn names(range: std::ops::Range<u64>) -> Vec<String> {
    range.map(|n| format!("m{n}")).collect()
}

#[test]
fn opening_asks_for_the_tail_and_follows() {
    let mut h = open(40, 6, Limits::default());
    assert_eq!(h.requests(), [Request::Tail, Request::Follow(true)]);
    h.draw();
    assert_eq!(h.log(), Vec::<String>::new(), "nothing before the tail");
}

#[test]
fn the_tail_shows_at_the_bottom_following() {
    let mut h = open(40, 6, Limits::default());
    h.requests();
    assert_eq!(h.send(tail(0..3, false)), Flow::Redraw);
    let mut want = vec!["--- start of log ---".to_owned()];
    want.extend(names(0..3));
    want.push("--- following ---".to_owned());
    assert_eq!(h.log(), want);
    assert!(
        h.screen()[0].starts_with("app  service  following"),
        "{:?}",
        h.screen()
    );
    // Followed lines show at the bottom.
    h.send(Event::Followed(lines(3..5)));
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
    assert_eq!(h.log()[h.log().len() - 2], "m4");
    assert_eq!(h.requests(), [], "the whole log is loaded");
}

#[test]
fn scrolling_up_pauses_following_and_the_bottom_resumes_it() {
    let mut h = open(40, 6, Limits::default());
    h.requests();
    h.send(tail(0..20, false));
    assert_eq!(h.log().first().map(String::as_str), Some("m16"));
    h.key(KeyCode::Char('k'));
    assert_eq!(h.log().first().map(String::as_str), Some("m15"));
    assert!(h.screen()[0].contains("following, paused"));
    // New lines don't move a paused view.
    h.send(Event::Followed(lines(20..22)));
    assert_eq!(h.log().first().map(String::as_str), Some("m15"));
    // Down to the bottom: following again.
    for _ in 0..2 {
        h.key(KeyCode::Char('j'));
    }
    assert!(h.screen()[0].contains("following, paused"));
    h.key(KeyCode::Down);
    assert!(!h.screen()[0].contains("paused"), "{:?}", h.screen());
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
    h.send(Event::Followed(lines(22..23)));
    assert_eq!(h.log()[h.log().len() - 2], "m22");
    assert_eq!(h.requests(), []);
}

#[test]
fn paging_keys_home_and_end_move_by_screens() {
    let mut h = open(40, 6, Limits::default());
    h.requests();
    h.send(tail(0..30, false));
    h.key(KeyCode::PageUp);
    assert_eq!(h.log(), names(21..26));
    h.key(KeyCode::Char('u'));
    assert_eq!(h.log(), names(16..21));
    h.key(KeyCode::PageDown);
    assert_eq!(h.log(), names(21..26));
    h.key(KeyCode::Char('d'));
    h.key(KeyCode::Char('d'));
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
    h.key(KeyCode::Home);
    assert_eq!(h.log()[0], "--- start of log ---");
    // Scrolling up from the top stays there.
    h.key(KeyCode::Up);
    assert_eq!(h.log()[0], "--- start of log ---");
    h.key(KeyCode::Char('g'));
    h.key(KeyCode::End);
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
    h.key(KeyCode::Char('g'));
    h.key(KeyCode::Char('G'));
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
    assert_eq!(h.requests(), [], "nothing more to load");
}

#[test]
fn near_the_oldest_line_the_page_before_is_asked_for_once() {
    let mut h = open(40, 6, Limits::default());
    h.requests();
    h.send(tail(100..110, true));
    assert_eq!(h.requests(), [Request::Older(position(100))]);
    // One at a time.
    h.key(KeyCode::Up);
    assert_eq!(h.requests(), []);
    h.send(older(90..100, true));
    assert_eq!(h.requests(), [Request::Older(position(90))]);
    // The view stays on the same lines when the page lands above it.
    let before = h.log();
    h.send(older(80..90, false));
    assert_eq!(h.log(), before);
    h.key(KeyCode::Home);
    assert_eq!(h.log()[0], "--- start of log ---");
    assert_eq!(h.log()[1], "m80");
    assert_eq!(h.requests(), []);
}

#[test]
fn far_from_the_oldest_line_nothing_is_asked_for() {
    let mut h = open(40, 4, Limits::default());
    h.requests();
    // 3 rows a screen, 5 screens: 15 rows above the window are enough.
    h.send(tail(0..30, true));
    assert_eq!(h.requests(), []);
}

#[test]
fn an_older_page_that_reached_nothing_ends_paging_back() {
    let mut h = open(40, 6, Limits::default());
    h.requests();
    h.send(tail(0..2, true));
    assert_eq!(h.requests(), [Request::Older(position(0))]);
    h.send(Event::Page {
        kind: PageKind::Older,
        lines: Vec::new(),
        oldest: None,
        newest: None,
        more_older: true,
        more_newer: true,
    });
    assert_eq!(h.requests(), []);
    assert_eq!(h.log()[0], "--- start of log ---");
}

#[test]
fn the_buffer_drops_the_far_end_past_its_entries() {
    let limits = Limits {
        entries: 10,
        bytes: usize::MAX,
    };
    let mut h = open(40, 6, limits);
    h.requests();
    h.send(tail(0..8, false));
    h.send(Event::Followed(lines(8..14)));
    // Following at the bottom: the oldest go, and there is more older.
    assert_eq!(h.viewer.entries.len(), 10);
    assert_eq!(h.requests(), [Request::Older(position(4))]);
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
}

#[test]
fn the_buffer_drops_the_far_end_past_its_bytes() {
    // Each message is 2 or 3 bytes ("m0"..): 12 bytes hold 6 lines or so.
    let limits = Limits {
        entries: usize::MAX,
        bytes: 12,
    };
    let mut h = open(40, 6, limits);
    h.requests();
    h.send(tail(0..4, false));
    h.send(Event::Followed(lines(4..10)));
    assert!(h.viewer.bytes <= 12);
    assert_eq!(h.viewer.entries.len(), 6);
    assert!(h.viewer.older.more);
}

#[test]
fn dropping_the_newest_end_stops_following() {
    let limits = Limits {
        entries: 12,
        bytes: usize::MAX,
    };
    let mut h = open(40, 4, limits);
    h.requests();
    h.send(tail(0..10, false));
    h.key(KeyCode::Home);
    h.send(Event::Followed(lines(10..14)));
    // The newest held line, m11, is within 5 screens: the page after it
    // is asked for at once.
    assert_eq!(
        h.requests(),
        [Request::Follow(false), Request::Newer(position(11))]
    );
    assert!(!h.viewer.following() && h.viewer.newer.more);
    // The view is where it was; End loads the tail and follows again.
    assert_eq!(h.log()[0], "--- start of log ---");
    h.key(KeyCode::End);
    assert_eq!(h.requests(), [Request::Tail, Request::Follow(true)]);
}

#[test]
fn near_the_newest_line_not_following_the_page_after_is_asked_for() {
    let limits = Limits {
        entries: 40,
        bytes: usize::MAX,
    };
    let mut h = open(40, 4, limits);
    h.requests();
    h.send(tail(0..38, false));
    h.key(KeyCode::Home);
    h.send(Event::Followed(lines(38..44)));
    assert_eq!(h.requests(), [Request::Follow(false)], "far from the end");
    // The newest held line is m39: scrolling near the bottom asks after it.
    for _ in 0..30 {
        h.key(KeyCode::Down);
    }
    assert_eq!(h.requests(), [Request::Newer(position(39))]);
    h.send(newer(40..44, false));
    h.key(KeyCode::End);
    assert_eq!(h.requests(), [Request::Follow(true)]);
    h.key(KeyCode::Char('f'));
    assert_eq!(h.requests(), [Request::Follow(false)]);
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- end of log ---")
    );
}

#[test]
fn f_stops_and_starts_the_follow() {
    let mut h = open(40, 6, Limits::default());
    h.requests();
    h.send(tail(0..3, false));
    h.key(KeyCode::Char('f'));
    assert_eq!(h.requests(), [Request::Follow(false)]);
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- end of log ---")
    );
    assert!(!h.screen()[0].contains("following"));
    h.key(KeyCode::Char('f'));
    assert_eq!(h.requests(), [Request::Follow(true)]);
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
    // Paused, `f` resumes rather than stops.
    h.send(tail(0..20, false));
    h.key(KeyCode::Up);
    h.key(KeyCode::Char('f'));
    assert_eq!(h.requests(), []);
    assert!(!h.screen()[0].contains("paused"));
}

/// The follow couldn't start: the view stops following where it is, and
/// one `f` retries.
#[test]
fn cant_follow_turns_the_follow_off_and_f_retries() {
    let mut h = open(60, 6, Limits::default());
    h.requests();
    h.send(tail(0..2, false));
    h.send(Event::Notice(Notice::CantFollow("no watches left".into())));
    assert!(!h.screen()[0].contains("following"), "{:?}", h.screen());
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- end of log ---")
    );
    h.key(KeyCode::Char('f'));
    assert_eq!(h.requests(), [Request::Follow(true)]);
}

/// After reading back (an older page) and following again, a notice is
/// the follow's: at the bottom, in sight.
#[test]
fn a_notice_while_following_after_an_older_page_goes_at_the_bottom() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(10..12, true));
    h.send(older(8..10, false));
    h.send(Event::Followed(lines(12..13)));
    h.send(Event::Notice(Notice::ReadErrors {
        first: "bad".into(),
        others: 0,
    }));
    assert!(matches!(h.viewer.entries.back(), Some(Entry::Notice(_))));
    h.send(older(6..8, false));
    h.send(Event::Notice(Notice::CantFollow("gone".into())));
    assert!(matches!(
        h.viewer.entries.back(),
        Some(Entry::Notice(Notice::CantFollow(_)))
    ));
}

/// A wrapped entry scrolled into, then the window widened or `t`
/// pressed: the entry has fewer rows than were scrolled past, and Down
/// still moves.
#[test]
fn scrolling_down_after_an_entry_shrank_still_moves() {
    for shrink in ["resize", "t"] {
        let mut h = open(20, 3, Limits::default());
        h.requests();
        let mut ls = lines(0..12);
        ls[1] = Line {
            time: Some(jiff::Timestamp::UNIX_EPOCH),
            ..line(1, &"x".repeat(70))
        };
        h.send(Event::Page {
            kind: PageKind::Tail,
            oldest: Some(position(0)),
            newest: Some(position(11)),
            lines: ls,
            more_older: false,
            more_newer: false,
        });
        // m1's entry is 5 rows at 20 columns with its time, 4 without.
        h.key(KeyCode::Home);
        for _ in 0..6 {
            h.key(KeyCode::Down);
        }
        assert_eq!(h.viewer.anchor, Anchor { item: 1, skip: 4 }, "{shrink}");
        if shrink == "resize" {
            h.terminal.backend_mut().resize(200, 5);
            h.terminal.autoresize().expect("resized");
            h.draw();
        } else {
            h.key(KeyCode::Char('t'));
        }
        let before = h.viewer.top();
        h.key(KeyCode::Down);
        assert!(
            h.viewer.top() != before || h.viewer.stuck(),
            "{shrink}: stuck at {before:?}"
        );
    }
}

#[test]
fn up_on_a_log_shorter_than_the_window_keeps_following() {
    let mut h = open(40, 10, Limits::default());
    h.requests();
    h.send(tail(0..3, false));
    h.key(KeyCode::Up);
    h.key(KeyCode::PageUp);
    h.key(KeyCode::Home);
    assert!(h.viewer.stuck());
    assert!(!h.screen()[0].contains("paused"));
}

#[test]
fn ctrl_and_alt_keys_are_left_alone() {
    let mut h = open(40, 6, Limits::default());
    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        let key = KeyEvent::new(KeyCode::Char('q'), modifiers);
        assert_eq!(h.viewer.input(Input::Key(key)), Flow::Unchanged);
    }
    assert_eq!(h.viewer.leave(), None);
}

#[test]
fn the_name_is_drawn_sanitized() {
    let viewer = Viewer::open(
        "ev\x1b]0;x\x07il\nmore",
        Kind::Container,
        format(),
        Arc::new(Notify::new()),
        |outbox| fake_source(outbox).0,
    );
    assert_eq!(viewer.name, "evil");
}

#[test]
fn notices_are_rows_of_their_own() {
    let mut h = open(60, 6, Limits::default());
    h.requests();
    h.send(tail(0..2, false));
    h.send(Event::Notice(Notice::CantFollow("no watches left".into())));
    assert!(
        h.log()
            .contains(&"can't follow: no watches left; f retries".to_owned()),
        "{:?}",
        h.log()
    );
}

/// A container's stop and start: the follow's notices, at the bottom
/// after an older page too, in the Muted role, and the view still
/// following; errors are in the Problem role.
#[test]
fn a_stop_and_start_are_muted_rows_at_the_bottom_while_following() {
    let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
    let mut h = open_with(
        60,
        8,
        Limits::default(),
        Format::new(TimeZone::UTC, theme.clone()),
    );
    h.requests();
    h.send(tail(10..12, true));
    h.send(older(8..10, false));
    h.send(Event::Notice(Notice::Stopped));
    h.send(Event::Notice(Notice::Started));
    h.send(Event::Notice(Notice::ReadFailed("refused".into())));
    let log = h.log();
    let n = log.len();
    assert_eq!(
        log[n - 3..],
        [
            "container stopped; waiting for it to start again",
            "container started",
            "--- following ---",
        ],
        "{log:?}"
    );
    assert!(h.screen()[0].contains("following"));
    // The page's error is the older page's: at the top.
    assert!(matches!(
        h.viewer.entries.front(),
        Some(Entry::Notice(Notice::ReadFailed(_)))
    ));
    let fg = |role| ratatui::style::Style::from(theme.style(role)).fg;
    let style_of = |item: isize| h.viewer.item_text(item).lines[0].style.fg;
    let len = isize::try_from(h.viewer.entries.len()).expect("a length");
    assert_eq!(style_of(len - 1), fg(Role::Muted), "started");
    assert_eq!(style_of(len - 2), fg(Role::Muted), "stopped");
    assert_eq!(style_of(0), fg(Role::Problem), "the error");
}

#[test]
fn a_notice_after_an_older_page_goes_at_the_top() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(10..12, true));
    h.send(older(8..10, false));
    h.send(Event::Notice(Notice::ReadErrors {
        first: "bad".into(),
        others: 0,
    }));
    assert!(matches!(h.viewer.entries.front(), Some(Entry::Notice(_))));
}

#[test]
fn events_of_an_old_generation_are_dropped() {
    let mut h = open(40, 6, Limits::default());
    h.requests();
    assert!(h.fake.send_stale(tail(0..3, false)));
    assert_eq!(h.viewer.wake(), Flow::Unchanged);
    h.draw();
    assert_eq!(h.log(), Vec::<String>::new());
    assert_eq!(h.viewer.wake(), Flow::Unchanged, "nothing waiting");
}

#[test]
fn w_toggles_wrap_and_t_the_time_and_origin() {
    let mut h = open(20, 6, Limits::default());
    h.requests();
    let mut long = line(0, "abcdefghijklmnopqrstuvwxyz");
    long.origin = b"app[7]".to_vec();
    h.send(Event::Page {
        kind: PageKind::Tail,
        lines: vec![long],
        oldest: Some(position(0)),
        newest: Some(position(0)),
        more_older: false,
        more_newer: false,
    });
    // ratatui wraps at word boundaries.
    assert_eq!(
        h.log(),
        [
            "--- start of log ---",
            "app[7]:",
            "abcdefghijklmnopqrst",
            "uvwxyz",
            "--- following ---"
        ]
    );
    h.key(KeyCode::Char('w'));
    assert_eq!(
        h.log(),
        [
            "--- start of log ---",
            "app[7]: abcdefghijkl",
            "--- following ---"
        ]
    );
    h.key(KeyCode::Char('t'));
    assert_eq!(
        h.log(),
        [
            "--- start of log ---",
            "abcdefghijklmnopqrstu",
            "--- following ---"
        ]
        .map(|s| s.chars().take(20).collect::<String>())
    );
}

#[test]
fn esc_goes_back_and_q_quits() {
    let mut h = open(40, 6, Limits::default());
    assert_eq!(h.viewer.leave(), None);
    assert_eq!(h.key(KeyCode::Esc), Flow::Quit);
    assert_eq!(h.viewer.leave(), Some(Leave::Back));
    let mut h = open(40, 6, Limits::default());
    assert_eq!(h.key(KeyCode::Char('q')), Flow::Quit);
    assert_eq!(h.viewer.leave(), Some(Leave::Quit));
    assert_eq!(h.key(KeyCode::Char('x')), Flow::Unchanged);
    assert_eq!(
        h.viewer.input(Input::FocusGained),
        Flow::Unchanged,
        "other input"
    );
}

/// A screen of service lines with time and origin, a message with a
/// newline, a cut record, a notice and the markers.
fn sample(width: u16, height: u16) -> Harness {
    let mut f = format();
    f.record_cap = 40;
    let mut h = open_with(width, height, Limits::default(), f);
    h.requests();
    let at = |n: u64, origin: &str, message: &str| Line {
        time: Some(
            jiff::Timestamp::from_second(1_794_560_645 + i64::try_from(n).expect("small"))
                .expect("a time"),
        ),
        origin: origin.as_bytes().to_vec(),
        ..line(n, message)
    };
    let ls = vec![
        at(0, "app[7]", "starting app"),
        at(1, "app[7]", "listening on port 8080"),
        at(2, "app[7]", "a message\nover two lines"),
        at(
            3,
            "app[7]",
            &"a record longer than the small record cap of 40 bytes".repeat(2),
        ),
        at(4, "systemd[1]", "app.service: Consumed 1.2s CPU time."),
    ];
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(4)),
        lines: ls,
        more_older: false,
        more_newer: false,
    });
    h.send(Event::Notice(Notice::ReadErrors {
        first: "/var/log/journal/x/system.journal: corrupt entry".into(),
        others: 2,
    }));
    h
}

#[test]
fn snapshot_following_120x40() {
    let h = sample(120, 40);
    assert_snapshot!(h.terminal.backend());
}

#[test]
fn snapshot_following_80x24() {
    let h = sample(80, 24);
    assert_snapshot!(h.terminal.backend());
}

#[test]
fn snapshot_wrap_off_not_following_80x24() {
    let mut h = sample(80, 24);
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::Char('f'));
    assert_snapshot!(h.terminal.backend());
}

#[test]
fn snapshot_paused_120x40() {
    let mut h = sample(120, 40);
    h.send(Event::Followed(lines(5..60)));
    h.key(KeyCode::PageUp);
    assert_snapshot!(h.terminal.backend());
}
