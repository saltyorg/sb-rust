//! The viewer against the fake source (tui spec, Testing Strategy):
//! scrolling, paging requests near both edges, follow paused by
//! scrolling and resumed at the bottom, the buffer's caps by entries and
//! by bytes, notices, events of an old generation dropped, and
//! `TestBackend` snapshots at 120×40 and 80×24.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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

    fn mouse(&mut self, kind: MouseEventKind) -> Flow {
        let flow = self.viewer.input(Input::Mouse(MouseEvent {
            kind,
            column: 3,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));
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

/// The wheel scrolls 3 rows (tui spec, "Scrolling"): up pauses the
/// follow, down to the bottom resumes it; other mouse events change
/// nothing.
#[test]
fn the_wheel_scrolls_three_rows() {
    let mut h = open(40, 6, Limits::default());
    h.requests();
    h.send(tail(0..20, false));
    assert_eq!(h.log().first().map(String::as_str), Some("m16"));
    assert_eq!(h.mouse(MouseEventKind::ScrollUp), Flow::Redraw);
    assert_eq!(h.log().first().map(String::as_str), Some("m13"));
    assert!(h.screen()[0].contains("following, paused"));
    assert_eq!(h.mouse(MouseEventKind::Moved), Flow::Unchanged);
    assert_eq!(
        h.mouse(MouseEventKind::Down(MouseButton::Left)),
        Flow::Unchanged
    );
    h.mouse(MouseEventKind::ScrollDown);
    assert_eq!(h.log().first().map(String::as_str), Some("m16"));
    assert!(!h.screen()[0].contains("paused"), "{:?}", h.screen());
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

// --- search (tui spec, "Search") and sideways scrolling ---

impl Harness {
    /// `/`, the line emptied (it opens on the search in effect), then
    /// `query` typed into it, then Enter.
    fn search(&mut self, query: &str) {
        self.key(KeyCode::Char('/'));
        for _ in 0..self.viewer.query.chars().count() {
            self.key(KeyCode::Backspace);
        }
        for c in query.chars() {
            self.key(KeyCode::Char(c));
        }
        self.key(KeyCode::Enter);
    }

    fn header(&self) -> String {
        self.screen()[0].clone()
    }

    fn mouse_with(&mut self, kind: MouseEventKind, modifiers: KeyModifiers) -> Flow {
        let flow = self.viewer.input(Input::Mouse(MouseEvent {
            kind,
            column: 3,
            row: 3,
            modifiers,
        }));
        self.draw();
        flow
    }
}

/// While following, a search starts at the newest match; in sight at the
/// bottom, the view stays and keeps following.
#[test]
fn a_search_while_following_starts_at_the_newest_match() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(0..50, false));
    h.search("m4");
    assert!(h.header().contains("/m4 11/11"), "{}", h.header());
    assert!(h.header().contains("following"), "{}", h.header());
    assert!(!h.header().contains("paused"), "{}", h.header());
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
    assert!(h.viewer.searching.is_none(), "Enter closes the line");
}

/// `n` and `N` move to the next and previous match, its row at the top,
/// and pause following; neither goes past an end.
#[test]
fn n_and_shift_n_jump_and_pause_following() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(0..50, false));
    h.search("m4");
    h.key(KeyCode::Char('N'));
    assert!(h.header().contains("/m4 10/11"), "{}", h.header());
    assert!(h.header().contains("following, paused"), "{}", h.header());
    assert_eq!(h.log()[0], "m48");
    h.key(KeyCode::Char('N'));
    assert_eq!(h.log()[0], "m47");
    h.key(KeyCode::Char('n'));
    h.key(KeyCode::Char('n'));
    assert!(h.header().contains("/m4 11/11"), "{}", h.header());
    assert_eq!(h.log()[0], "m49");
    h.key(KeyCode::Char('n'));
    assert!(
        h.header().contains("11/11"),
        "the last stays: {}",
        h.header()
    );
    for _ in 0..12 {
        h.key(KeyCode::Char('N'));
    }
    assert!(h.header().contains("/m4 1/11"), "{}", h.header());
    assert_eq!(h.log()[0], "m4");
}

/// Not following, a search starts at the first match from the window's
/// top, and a match in sight doesn't move the view.
#[test]
fn a_search_not_following_starts_from_the_top_in_sight() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(0..50, false));
    h.key(KeyCode::Home);
    let before = h.log();
    h.search("m3");
    assert!(h.header().contains("/m3 1/11"), "{}", h.header());
    assert_eq!(h.log(), before, "m3 is in sight");
    h.key(KeyCode::Char('n'));
    assert_eq!(h.log()[0], "m30");
    // Out of sight, the view moves to the first match below the top.
    h.key(KeyCode::Home);
    h.search("m2");
    assert!(h.header().contains("/m2 1/11"), "{}", h.header());
    // With nothing at or below the top, the last match above it.
    h.key(KeyCode::End);
    h.key(KeyCode::Char('f'));
    h.key(KeyCode::PageUp);
    h.key(KeyCode::PageUp);
    h.search("m1");
    // m1 and m10 to m19 are all above the window: the last of them.
    assert!(h.header().contains("/m1 11/11"), "{}", h.header());
    assert_eq!(h.log()[0], "m19");
}

#[test]
fn smart_case_counts_in_the_viewer_and_no_match_says_so() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(2)),
        lines: vec![
            line(0, "Error: one"),
            line(1, "error: two"),
            line(2, "ERROR"),
        ],
        more_older: false,
        more_newer: false,
    });
    h.search("error");
    assert!(h.header().contains("/error 3/3"), "{}", h.header());
    h.search("Error");
    assert!(h.header().contains("/Error 1/1"), "{}", h.header());
    h.search("zzz");
    assert!(h.header().contains("/zzz no matches"), "{}", h.header());
}

/// Esc clears a search, then leaves; Esc in the line clears it too, and an
/// empty line's Enter clears it.
#[test]
fn esc_clears_the_search_then_goes_back() {
    let mut h = open(100, 8, Limits::default());
    h.requests();
    h.send(tail(0..10, false));
    h.search("m1");
    assert!(h.header().contains("Esc clear"), "{}", h.header());
    assert_eq!(h.key(KeyCode::Esc), Flow::Redraw);
    assert!(!h.header().contains("/m1"), "{}", h.header());
    assert_eq!(h.viewer.leave(), None);
    h.search("m1");
    h.key(KeyCode::Char('/'));
    assert!(h.screen().iter().any(|r| r.contains("Search: m1")));
    h.key(KeyCode::Esc);
    assert!(!h.header().contains("/m1"), "{}", h.header());
    assert!(h.viewer.searching.is_none());
    h.search("m1");
    h.key(KeyCode::Char('/'));
    h.key(KeyCode::Backspace);
    h.key(KeyCode::Backspace);
    h.key(KeyCode::Enter);
    assert!(!h.header().contains("/m"), "{}", h.header());
    assert_eq!(h.key(KeyCode::Esc), Flow::Quit);
    assert_eq!(h.viewer.leave(), Some(Leave::Back));
}

/// The line takes every key while open: `q` is a letter there, and Ctrl-M
/// ends it as Enter does.
#[test]
fn the_search_line_takes_every_key() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(1)),
        lines: vec![line(0, "quit"), line(1, "other")],
        more_older: false,
        more_newer: false,
    });
    h.key(KeyCode::Char('/'));
    assert_eq!(h.key(KeyCode::Char('q')), Flow::Redraw);
    h.viewer.input(Input::Key(KeyEvent::new(
        KeyCode::Char('m'),
        KeyModifiers::CONTROL,
    )));
    h.draw();
    assert!(h.header().contains("/q 1/1"), "{}", h.header());
}

/// Lines that come in are counted: older pages before the current match,
/// followed lines after it; a match dropped with its entry shows `-`.
#[test]
fn matches_are_counted_as_lines_come_and_go() {
    let mut h = open(
        60,
        8,
        Limits {
            entries: 12,
            bytes: 1 << 20,
        },
    );
    h.requests();
    h.send(tail(40..50, true));
    h.search("m4");
    assert!(h.header().contains("/m4 10/10"), "{}", h.header());
    h.send(Event::Followed(lines(50..52)));
    assert!(h.header().contains("/m4 10/10"), "{}", h.header());
    h.search("m");
    assert!(h.header().contains("/m 12/12"), "{}", h.header());
    // Paging back drops the newest end, the current match with it.
    h.key(KeyCode::Char('g'));
    h.send(older(30..40, true));
    assert!(h.header().contains("/m -/12"), "{}", h.header());
}

/// The current match keeps its place when an older page goes in before
/// it.
#[test]
fn an_older_page_moves_the_current_match_along() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.search("m4");
    h.key(KeyCode::Char('N'));
    assert!(h.header().contains("/m4 9/10"), "{}", h.header());
    h.send(older(0..40, false));
    assert!(h.header().contains("/m4 10/11"), "{}", h.header());
    h.key(KeyCode::Char('n'));
    assert_eq!(h.log()[0], "m49");
}

/// Every match is lit in the `SearchHit` role, a match across the log's
/// own colours whole.
#[test]
fn matches_are_lit_in_the_search_hit_role() {
    let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
    let hit = theme.color(Role::SearchHit).map(|c| {
        let style = Style::from(anstyle::Style::new().bg_color(Some(c)));
        style.bg.expect("a background")
    });
    let mut h = open_with(60, 8, Limits::default(), Format::new(TimeZone::UTC, theme));
    h.requests();
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(0)),
        lines: vec![line(0, "\x1b[32m[Info]\x1b[0m Housekeeping")],
        more_older: false,
        more_newer: false,
    });
    h.search("info] house");
    let buffer = h.terminal.backend().buffer();
    let row = (0..buffer.area.height)
        .find(|&y| buffer[(0, y)].symbol() == "[")
        .expect("the line");
    let lit: String = (0..buffer.area.width)
        .filter(|&x| buffer[(x, row)].bg == hit.expect("a colour"))
        .map(|x| buffer[(x, row)].symbol().to_owned())
        .collect();
    assert_eq!(lit, "Info] House");
}

/// With wrap off, ←/→, Shift+wheel and the horizontal wheel step 8
/// columns and the header shows the column; with wrap on they don't.
#[test]
fn sideways_steps_with_wrap_off() {
    let mut h = open(60, 6, Limits::default());
    h.requests();
    let wide = "0123456789abcdefghijklmnopqrstuvwxyz".repeat(3);
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(0)),
        lines: vec![line(0, &wide)],
        more_older: false,
        more_newer: false,
    });
    assert_eq!(h.key(KeyCode::Right), Flow::Unchanged, "wrap on");
    assert_eq!(
        h.mouse(MouseEventKind::ScrollRight),
        Flow::Unchanged,
        "wrap on"
    );
    h.key(KeyCode::Char('w'));
    assert!(h.header().contains("col 1"), "{}", h.header());
    h.key(KeyCode::Right);
    assert!(h.header().contains("col 9"), "{}", h.header());
    assert!(
        h.log().iter().any(|r| r.starts_with("89abcdefghij")),
        "{:?}",
        h.log()
    );
    h.mouse(MouseEventKind::ScrollRight);
    h.mouse_with(MouseEventKind::ScrollDown, KeyModifiers::SHIFT);
    assert!(h.header().contains("col 25"), "{}", h.header());
    h.mouse(MouseEventKind::ScrollLeft);
    h.mouse_with(MouseEventKind::ScrollUp, KeyModifiers::SHIFT);
    h.key(KeyCode::Left);
    assert!(h.header().contains("col 1"), "{}", h.header());
    h.key(KeyCode::Left);
    assert!(h.header().contains("col 1"), "no further left");
    h.key(KeyCode::Right);
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::Char('w'));
    assert!(h.header().contains("col 1"), "w resets it: {}", h.header());
}

/// With wrap off, a jump brings a match off to the right into view.
#[test]
fn a_jump_with_wrap_off_scrolls_to_the_match() {
    let mut h = open(60, 6, Limits::default());
    h.requests();
    let long = format!("{}needle", "x".repeat(100));
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(1)),
        lines: vec![line(0, &long), line(1, "needle at the start")],
        more_older: false,
        more_newer: false,
    });
    h.key(KeyCode::Char('w'));
    h.search("needle");
    h.key(KeyCode::Char('N'));
    // The match at column 101 goes to the window's middle: 101 - 30,
    // down to a step of 8, is 64, shown as column 65.
    assert!(h.header().contains("col 65"), "{}", h.header());
    assert!(
        h.log().iter().any(|r| r.ends_with("xxxxneedle")),
        "{:?}",
        h.log()
    );
    h.key(KeyCode::Char('n'));
    assert!(h.header().contains("col 1"), "{}", h.header());
}

/// With wrap on, a jump puts the match's row at the window's top, also
/// inside a long wrapped entry.
#[test]
fn a_jump_finds_its_row_inside_a_wrapped_entry() {
    let mut h = open(20, 6, Limits::default());
    h.requests();
    let long = format!("{} needle", "word ".repeat(20).trim_end());
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(1)),
        lines: vec![line(0, &long), line(1, "last")],
        more_older: false,
        more_newer: false,
    });
    h.key(KeyCode::Home);
    h.search("needle");
    h.key(KeyCode::Char('N'));
    h.key(KeyCode::Char('n'));
    assert!(h.log()[0].contains("needle"), "{:?}", h.log());
}

#[test]
fn snapshot_search_hits_120x40() {
    let mut h = sample(120, 40);
    h.search("app");
    assert_snapshot!(h.terminal.backend());
}

#[test]
fn snapshot_search_line_open_80x24() {
    let mut h = sample(80, 24);
    h.key(KeyCode::Char('/'));
    for c in "lines".chars() {
        h.key(KeyCode::Char(c));
    }
    assert_snapshot!(h.terminal.backend());
}

/// `N` while following, to a match already on the window's top row: the
/// view stays where it is, paused (it once jumped to a stale anchor).
#[test]
fn a_jump_while_following_to_the_top_row_keeps_the_view() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    let ls: Vec<Line> = (0..50)
        .map(|n| line(n, if n == 44 || n == 49 { "hit" } else { "x" }))
        .collect();
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(49)),
        lines: ls,
        more_older: false,
        more_newer: false,
    });
    h.search("hit");
    assert!(h.header().contains("/hit 2/2"), "{}", h.header());
    let before = h.log();
    assert_eq!(before[0], "hit");
    h.key(KeyCode::Char('N'));
    assert!(h.header().contains("/hit 1/2"), "{}", h.header());
    assert!(h.header().contains("paused"), "{}", h.header());
    assert_eq!(h.log(), before);
}

/// Several matches in one entry: `n` and `N` step through them.
#[test]
fn n_steps_through_the_matches_of_one_entry() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: Some(position(0)),
        newest: Some(position(1)),
        lines: vec![line(0, "ab ab ab"), line(1, "none")],
        more_older: false,
        more_newer: false,
    });
    h.search("ab");
    assert!(h.header().contains("/ab 3/3"), "{}", h.header());
    h.key(KeyCode::Char('N'));
    h.key(KeyCode::Char('N'));
    assert!(h.header().contains("/ab 1/3"), "{}", h.header());
    h.key(KeyCode::Char('n'));
    assert!(h.header().contains("/ab 2/3"), "{}", h.header());
}

/// Entries dropped from the front move the current match back, and it
/// goes with its entry; a notice put in front moves it along.
#[test]
fn the_current_match_follows_the_fronts_changes() {
    let cap = Limits {
        entries: 12,
        bytes: 1 << 20,
    };
    let mut h = open(60, 8, cap);
    h.requests();
    h.send(tail(40..50, true));
    h.search("m4");
    assert!(h.header().contains("/m4 10/10"), "{}", h.header());
    h.send(Event::Followed(lines(50..53)));
    assert!(h.header().contains("/m4 9/9"), "{}", h.header());
    assert_eq!(h.viewer.current, Some((8, 0)));
    h.send(Event::Followed(lines(53..55)));
    assert!(h.header().contains("/m4 7/7"), "{}", h.header());
    assert_eq!(h.viewer.current, Some((6, 0)));
    h.send(Event::Followed(lines(55..62)));
    assert_eq!(h.viewer.current, None, "gone with its entry");
    assert!(h.header().contains("/m4 no matches"), "{}", h.header());

    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.key(KeyCode::Char('g'));
    h.search("m45");
    assert_eq!(h.viewer.current, Some((5, 0)));
    h.send(older(30..40, true));
    assert_eq!(h.viewer.current, Some((15, 0)));
    h.send(Event::Notice(Notice::ReadFailed("gone".into())));
    assert_eq!(h.viewer.current, Some((16, 0)), "a notice in front");
    assert!(h.header().contains("/m45 1/1"), "{}", h.header());
}

/// A tail page (End with newer lines unloaded) reloads the buffer; the
/// search starts again at the newest match.
#[test]
fn a_tail_page_picks_the_newest_match_again() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, false));
    h.search("m4");
    h.send(tail(40..48, false));
    assert!(h.header().contains("/m4 8/8"), "{}", h.header());
}
