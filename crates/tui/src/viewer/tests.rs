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
use ratatui::style::{Color, Modifier};
use sb_cli_core::theme::{Palette, Profile, Theme};

use super::*;
use crate::ahead::Ahead;
use crate::source::{Found, Order, SearchEnd, Side};
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

/// A viewer whose header and log take `height` rows, on a terminal one
/// row taller for the footer (the key map's footer row, tui spec "Keys",
/// Hints), so the tests count the log's rows as before it.
fn open(width: u16, height: u16, limits: Limits) -> Harness {
    open_with(width, height + 1, limits, format())
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

    /// The log rows (the screen without its header and its footer),
    /// blank ones dropped.
    fn log(&self) -> Vec<String> {
        let screen = self.screen();
        let rows = screen.len().saturating_sub(1);
        screen
            .into_iter()
            .take(rows)
            .skip(1)
            .filter(|row| !row.is_empty())
            .collect()
    }

    /// The footer row.
    fn footer(&self) -> String {
        self.screen().pop().unwrap_or_default()
    }
}

impl Harness {
    /// The source answers the viewer's last search: the older side's
    /// matches at `older` (nearest first), then its end.
    fn search_answered(&mut self, older: &[u64]) {
        let id = self
            .requests()
            .into_iter()
            .rev()
            .find_map(|r| match r {
                Request::Search(search) => Some(search.id),
                _ => None,
            })
            .expect("a search asked for");
        self.send(Event::Found {
            search: id,
            side: Side::Older,
            order: Order::NearestFirst,
            found: older
                .iter()
                .map(|&n| Found {
                    position: position(n),
                    time: None,
                    count: 1,
                })
                .collect(),
            reached: None,
            end: Some(SearchEnd::Done),
        });
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
    h.key(KeyCode::Char('b'));
    assert_eq!(h.log(), names(16..21));
    // `u` and `d` no longer page (the key map's decision 1).
    assert_eq!(h.key(KeyCode::Char('u')), Flow::Unchanged);
    assert_eq!(h.key(KeyCode::Char('d')), Flow::Unchanged);
    assert_eq!(h.log(), names(16..21));
    h.key(KeyCode::PageDown);
    assert_eq!(h.log(), names(21..26));
    h.key(KeyCode::Char(' '));
    h.key(KeyCode::Char(' '));
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

/// Rows above the window inside a tall entry count with the entries
/// above it: the page before is asked for once both make less than five
/// screens, and not before.
#[test]
fn rows_inside_a_tall_entry_count_toward_the_oldest_line() {
    // Two log rows: five screens are 10 rows.
    let mut h = open(40, 3, Limits::default());
    let mut ls = lines(0..5);
    // 30 rows at 40 columns.
    ls[4] = line(4, &"x".repeat(1200));
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: true,
        more_newer: false,
    });
    h.requests();
    // The window starts 29 rows into the tall entry; 23 rows up leaves
    // 6 of its rows and the 4 short entries above: 10, not less.
    for _ in 0..23 {
        h.key(KeyCode::Char('k'));
    }
    assert!(
        !h.requests().iter().any(|r| matches!(r, Request::Older(_))),
        "asked too soon"
    );
    h.key(KeyCode::Char('k'));
    assert!(
        h.requests().iter().any(|r| matches!(r, Request::Older(_))),
        "not asked at 9 rows"
    );
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

/// An origin's bytes count toward the byte cap as the message's do.
#[test]
fn the_byte_cap_counts_origins_too() {
    let limits = Limits {
        entries: usize::MAX,
        bytes: 40,
    };
    let mut h = open(40, 6, limits);
    h.requests();
    let with_origin = |n: u64| Line {
        origin: vec![b'o'; 8],
        ..line(n, "m")
    };
    h.send(tail(0..1, false));
    h.send(Event::Followed((1..11).map(with_origin).collect()));
    // Nine bytes a line: four fit in 40.
    assert_eq!(h.viewer.entries.len(), 4);
    assert!(h.viewer.bytes <= 40);
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
    assert!(h.footer().contains("Esc clear"), "{}", h.footer());
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
    h.key(KeyCode::PageUp);
    h.key(KeyCode::PageUp);
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
    // The whole-log search found m4 before the buffer.
    h.search_answered(&[4]);
    h.key(KeyCode::Char('N'));
    assert!(h.header().contains("/m4 10/11"), "{}", h.header());
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
    assert!(
        (0..buffer.area.width).all(|x| !buffer[(x, row)].modifier.contains(Modifier::REVERSED)),
        "no reverse video under truecolor"
    );
}

/// Under 16 colours and under ascii every match is drawn in reverse
/// video, not on `SearchHit`'s background (the maintainer, 2026-11-14):
/// the 16-colour pick, yellow, hid a yellow span inside a match, which
/// now keeps its colour, reversed.
#[test]
fn matches_are_reversed_in_sixteen_colours_and_ascii() {
    for (profile, fg) in [
        (Profile::Ansi, Color::Yellow),
        (Profile::Ascii, Color::Reset),
    ] {
        let theme = Theme::resolve(&Palette::default(), profile);
        let mut h = open_with(60, 8, Limits::default(), Format::new(TimeZone::UTC, theme));
        h.requests();
        h.send(Event::Page {
            kind: PageKind::Tail,
            oldest: Some(position(0)),
            newest: Some(position(0)),
            lines: vec![line(0, "\x1b[33m[Warn]\x1b[0m disk low")],
            more_older: false,
            more_newer: false,
        });
        h.search("warn] d");
        let buffer = h.terminal.backend().buffer();
        let row = (0..buffer.area.height)
            .find(|&y| buffer[(0, y)].symbol() == "[")
            .expect("the line");
        let lit: String = (0..buffer.area.width)
            .filter(|&x| buffer[(x, row)].modifier.contains(Modifier::REVERSED))
            .map(|x| buffer[(x, row)].symbol().to_owned())
            .collect();
        assert_eq!(lit, "Warn] d", "{profile:?}");
        let w = &buffer[(1, row)];
        assert_eq!((w.fg, w.bg), (fg, Color::Reset), "{profile:?}");
    }
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
    h.search_answered(&[]);
    assert!(h.header().contains("/m4 10/10"), "{}", h.header());
    // The count is the log's (tui spec, "Whole-log search"): a match
    // the buffer drops is still one.
    h.send(Event::Followed(lines(50..53)));
    assert!(h.header().contains("/m4 10/10"), "{}", h.header());
    assert_eq!(h.viewer.current, Some((8, 0)));
    h.send(Event::Followed(lines(53..55)));
    assert!(h.header().contains("/m4 10/10"), "{}", h.header());
    assert_eq!(h.viewer.current, Some((6, 0)));
    h.send(Event::Followed(lines(55..62)));
    assert_eq!(h.viewer.current, None, "gone with its entry");
    assert!(h.header().contains("/m4 -/10"), "{}", h.header());

    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.key(KeyCode::Char('g'));
    h.search("m45");
    h.search_answered(&[]);
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
    // The newest match in the buffer, numbered among the log's ten.
    assert_eq!(h.viewer.current, Some((7, 0)));
    assert!(h.header().contains("/m4 8/10"), "{}", h.header());
}

// --- no save and no print (tui spec, decision 24) ---

/// `s` and `p` are no keys of the viewer: neither saves nor quits.
#[test]
fn s_and_p_do_nothing() {
    let mut h = open(80, 10, Limits::default());
    h.send(tail(1..3, false));
    assert_eq!(h.key(KeyCode::Char('s')), Flow::Unchanged);
    assert_eq!(h.key(KeyCode::Char('p')), Flow::Unchanged);
    assert_eq!(h.viewer.leave(), None);
}

/// A tail of `n` records of 5,000+ characters each, in words, so a
/// wrapped record takes many rows.
fn long_tail(n: u64) -> Event {
    let words = "lorem ipsum ".repeat(420);
    let ls: Vec<Line> = (0..n).map(|i| line(i, &format!("m{i} {words}"))).collect();
    Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: true,
        more_newer: false,
    }
}

/// What each of a run of frames costs on a buffer of `n` long records at
/// 240×70: the entries measured, the texts built, and the time taken (a
/// key's handling, paging requests included, and its frame).
fn frame_costs(n: u64) -> Vec<(usize, usize, std::time::Duration)> {
    frame_costs_searching(n, false)
}

/// What the frame Home's page brings costs (tui spec, "Home and End"):
/// a buffer of 20,000 long records paged up, Home, then the log's first
/// 500 records as its page, the frame's texts built and its time.
fn home_page_cost() -> (usize, std::time::Duration) {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(20_000));
    h.key(KeyCode::PageUp);
    h.key(KeyCode::Home);
    let Event::Page { lines, newest, .. } = long_tail(500) else {
        unreachable!("a page")
    };
    h.viewer.built.set(0);
    let started = std::time::Instant::now();
    h.send(Event::Page {
        kind: PageKind::Head,
        oldest: lines.first().map(|l| l.position.clone()),
        newest,
        lines,
        more_older: false,
        more_newer: true,
    });
    (h.viewer.built.get(), started.elapsed())
}

/// [`frame_costs`], with a whole-log search reading when `searching`:
/// 9,999 matches outside the buffer known and the progress in the
/// header, its query matching none of the buffer (tui spec, "Whole-log
/// search", "Frame budget").
fn frame_costs_searching(n: u64, searching: bool) -> Vec<(usize, usize, std::time::Duration)> {
    // Room for every record: the cap by bytes would drop most of them.
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(n));
    if searching {
        h.requests();
        h.search("no such text");
        let id = searches(&h.requests()).0[0].id;
        // One short of the cap, so the side still reads.
        let older: Vec<u64> = (1..crate::source::MATCHES as u64)
            .map(|i| 1_000_000 + i)
            .collect();
        h.send(Event::Found {
            search: id,
            side: Side::Older,
            order: Order::NearestFirst,
            found: found_at(&older),
            reached: Some(Timestamp::now() - jiff::SignedDuration::from_hours(49)),
            end: None,
        });
        assert!(
            h.header().contains("searching… 2 days back"),
            "{}",
            h.header()
        );
    }
    let keys = [
        // Following at the bottom, the layout changed (`t` off, on).
        KeyCode::Char('t'),
        KeyCode::Char('t'),
        // Paused, a few screens up, then near the top.
        KeyCode::PageUp,
        KeyCode::PageUp,
        KeyCode::Char('k'),
        KeyCode::Home,
        KeyCode::PageDown,
        // Wrap off (the switch frame), scrolled and sideways, then on
        // again at the bottom.
        KeyCode::Char('w'),
        KeyCode::PageUp,
        KeyCode::Char('k'),
        KeyCode::Right,
        KeyCode::Left,
        KeyCode::Char('w'),
        KeyCode::End,
    ];
    keys.iter()
        .map(|&key| {
            h.viewer.measured.set(0);
            h.viewer.built.set(0);
            let started = std::time::Instant::now();
            h.key(key);
            (
                h.viewer.measured.get(),
                h.viewer.built.get(),
                started.elapsed(),
            )
        })
        .collect()
}

/// A tail of `n` records of `words`, each after its number.
fn tail_of(n: u64, words: &str) -> Event {
    let ls: Vec<Line> = (0..n).map(|i| line(i, &format!("m{i} {words}"))).collect();
    Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: true,
        more_newer: false,
    }
}

/// The far-right timing's records, each 4,500 columns and more: words,
/// CJK text with a space every six characters, and words each in its
/// own colour.
fn far_records() -> [(&'static str, String); 3] {
    let colours: String = (0..700)
        .map(|i| format!("\x1b[3{}mword{i} ", i % 8))
        .collect::<Vec<_>>()
        .concat();
    [
        ("plain", "lorem ipsum ".repeat(420)),
        ("cjk", "日本語のログ ".repeat(360)),
        ("colour", colours),
    ]
}

/// What one frame cost: the texts built, the widest screen line handed
/// to ratatui, the message bytes parsed far right, and the time taken.
#[derive(Clone, Copy, Debug)]
struct Cost {
    built: usize,
    widest: usize,
    parsed: usize,
    time: std::time::Duration,
}

/// What frames cost with wrap off at `column`, on 2,000 records of
/// `words` at 240×70 in `format`: a step right onto the column, a row
/// up, a step left; then frames that build every text in view again
/// there, from their lines' kept resume places: `t` off and on, a
/// search, a resize (from 160 columns: narrower, the texts kept hold
/// the window still and are drawn again as they are). Last, one that
/// builds them with nothing kept, and nothing made ahead
/// ([`first_sight_costs`] has those).
fn far_right_costs(column: u16, words: &str, format: Format) -> Vec<Cost> {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    // 69 log rows, as before the footer's row (the key map's footer).
    let mut h = open_with(240, 71, limits, format);
    h.send(tail_of(2_000, words));
    // Nothing made ahead: the rebuilds work from the places their own
    // texts kept, and the worker would only make texts for windows the
    // next frames change again.
    h.viewer.ahead = Ahead::default();
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::PageUp);
    h.viewer.column = column.saturating_sub(COLUMN_STEP);
    h.draw();
    let cost = |h: &mut Harness, frame: &dyn Fn(&mut Harness)| {
        h.viewer.built.set(0);
        h.viewer.widest.set(0);
        h.viewer.parsed.set(0);
        let started = std::time::Instant::now();
        frame(h);
        let time = started.elapsed();
        Cost {
            built: h.viewer.built.get(),
            widest: h.viewer.widest.get(),
            parsed: h.viewer.parsed.get(),
            time,
        }
    };
    let mut costs: Vec<_> = [
        KeyCode::Right,
        KeyCode::Char('k'),
        KeyCode::Left,
        KeyCode::Char('t'),
        KeyCode::Char('t'),
    ]
    .iter()
    .map(|&key| {
        cost(&mut h, &|h| {
            h.key(key);
        })
    })
    .collect();
    // The frame after a search: its own work, counting matches in every
    // entry, isn't the frame's.
    h.viewer.query = "lorem".to_owned();
    costs.push(cost(&mut h, &|h| h.draw()));
    // A resize to 240 columns from 160, the texts built there: wider, the
    // window needs more columns than they hold.
    h.terminal.backend_mut().resize(160, 71);
    h.key(KeyCode::Char('t'));
    h.key(KeyCode::Char('t'));
    h.terminal.backend_mut().resize(240, 71);
    costs.push(cost(&mut h, &|h| h.draw()));
    // And with nothing kept or made ahead at all, as for a line seen there
    // for the first time without places made ahead.
    costs.push(cost(&mut h, &|h| {
        h.viewer.drawn = Drawn::default();
        h.draw();
    }));
    costs
}

/// What frames that bring in lines seen far right for the first time
/// cost at `column`, on 2,000 records of `words` at 240×70 in `format`:
/// a page up, Home and End, each after the worker made its places and
/// texts ahead (as the time to a person's next key lets it).
fn first_sight_costs(column: u16, words: &str, format: Format) -> Vec<Cost> {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    // 69 log rows, as before the footer's row (the key map's footer).
    let mut h = open_with(240, 71, limits, format);
    // The log's start loaded: Home moves within the buffer, as it did
    // before it could ask for the log's first page.
    let Event::Page {
        lines,
        oldest,
        newest,
        more_newer,
        ..
    } = tail_of(2_000, words)
    else {
        unreachable!("a page")
    };
    h.send(Event::Page {
        kind: PageKind::Tail,
        lines,
        oldest,
        newest,
        more_older: false,
        more_newer,
    });
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::PageUp);
    h.viewer.column = column;
    h.draw();
    [KeyCode::PageUp, KeyCode::Home, KeyCode::End]
        .iter()
        .map(|&key| {
            h.viewer.settle_ahead();
            h.viewer.built.set(0);
            h.viewer.widest.set(0);
            h.viewer.parsed.set(0);
            let started = std::time::Instant::now();
            h.key(key);
            let time = started.elapsed();
            Cost {
                built: h.viewer.built.get(),
                widest: h.viewer.widest.get(),
                parsed: h.viewer.parsed.get(),
                time,
            }
        })
        .collect()
}

/// Lines seen for the first time far right (the follow-up "First sight
/// far right meets 5 ms") build nothing at columns 1,000 and 4,000:
/// their texts were made ahead for the window, so a page up, Home or End
/// costs what a step sideways does. The time of each frame is printed,
/// for the by-hand release timing (the size ADR).
fn first_sight_holds(name: &str, words: &str, format: &Format) {
    for column in [1_000, 4_000] {
        let costs = first_sight_costs(column, words, format.clone());
        eprintln!(
            "{name} first sight at column {column}, page up, Home, End: {:?}",
            costs.iter().map(|c| c.time).collect::<Vec<_>>()
        );
        for c in &costs {
            assert_eq!((c.built, c.parsed), (0, 0), "{name} at {column}: {costs:?}");
            assert!(c.widest <= 2 * 240, "{name} at {column}: {costs:?}");
        }
    }
}

#[test]
fn first_sight_far_right_builds_nothing_with_colour_off() {
    first_sight_holds("ascii", &"lorem ipsum ".repeat(420), &format());
}

/// [`first_sight_holds`] for the far-right records' kind `at`, in
/// truecolor: one test each, so each stays short in a debug build.
fn first_sight_holds_for(at: usize) {
    let truecolor = Format::new(
        TimeZone::UTC,
        Theme::resolve(&Palette::default(), Profile::TrueColor),
    );
    let (name, words) = &far_records()[at];
    first_sight_holds(name, words, &truecolor);
}

#[test]
fn first_sight_far_right_builds_nothing_on_plain_records() {
    first_sight_holds_for(0);
}

#[test]
fn first_sight_far_right_builds_nothing_on_cjk_records() {
    first_sight_holds_for(1);
}

#[test]
fn first_sight_far_right_builds_nothing_on_colour_per_word_records() {
    first_sight_holds_for(2);
}

/// The far-right frames of [`far_right_costs`] at columns 1,000 and
/// 4,000 against those at the first column, for `words` in `format`: the
/// same texts built, each screen line handed to ratatui cut to about the
/// window, and each frame that builds the texts again from their resume
/// places parsing about as much at 4,000 as at 1,000, a few screens of
/// each line in view, so that its cost doesn't grow with the column.
fn far_right_holds(name: &str, words: &str, format: &Format) {
    let built = |costs: &[Cost]| costs.iter().map(|c| c.built).collect::<Vec<_>>();
    let near = far_right_costs(0, words, format.clone());
    eprintln!("{name} wrap off at column 0: {near:?}");
    let mut parsed = Vec::new();
    for column in [1_000, 4_000] {
        let far = far_right_costs(column, words, format.clone());
        eprintln!(
            "{name} wrap off at column {column}, right, up, left, t, t, search, resize, from nothing: {:?}",
            far.iter().map(|c| c.time).collect::<Vec<_>>()
        );
        // The same texts built as at the first column, but for the resize:
        // at the first column the texts built at 160 hold 240 already.
        assert_eq!(
            built(&far)[..6],
            built(&near)[..6],
            "{name} at {column}: {far:?}"
        );
        // Each screen line handed to ratatui is about the window: cut at
        // the column, or built from a little left of it through a little
        // right ([`resume::far`]).
        assert!(
            far.iter().all(|c| c.widest <= 2 * 240),
            "{name} at {column}: {far:?}"
        );
        // A rebuild parses about a screen and a half of each of 69 lines:
        // the window, a quarter of a screen right of it, an eighth left,
        // and up to a resume's spacing more, at a byte or two a column
        // (from nothing, at 1,000, it parses about 2,600 bytes a line).
        for (i, c) in far[3..7].iter().enumerate() {
            assert!(c.built <= 69, "{name} at {column}, rebuild {i}: {far:?}");
            assert!(
                c.parsed <= 69 * 1000,
                "{name} at {column}, rebuild {i}: {far:?}"
            );
        }
        assert!(
            far[3..7].iter().all(|c| c.built == 69),
            "{name} at {column}: {far:?}"
        );
        parsed.push(far[3..7].iter().map(|c| c.parsed).collect::<Vec<_>>());
    }
    for (at_1000, at_4000) in parsed[0].iter().zip(&parsed[1]) {
        assert!(
            at_4000 * 4 <= at_1000 * 5,
            "{name}: {at_4000} bytes at 4,000 against {at_1000} at 1,000"
        );
    }
}

/// The frame cost (tui spec, Testing Strategy, "Frame cost"; proto-logs'
/// "frame-time budget check"): the log lines measured and the texts built
/// per frame don't grow with the buffer, here 2,000 against 20,000
/// records of 5,000+ characters; the lines measured stay within a few
/// screens of entries, and the texts built within the window, with wrap
/// on and off and for the frame that turns wrap off. The time of each
/// frame is printed, for the by-hand release timing (`cargo test
/// --release`, the size ADR).
#[test]
fn a_frames_cost_doesnt_grow_with_the_buffer() {
    let small = frame_costs(2_000);
    let big = frame_costs(20_000);
    let counts = |costs: &[(usize, usize, std::time::Duration)]| {
        costs.iter().map(|(count, _, _)| *count).collect::<Vec<_>>()
    };
    let builds = |costs: &[(usize, usize, std::time::Duration)]| {
        costs.iter().map(|(_, built, _)| *built).collect::<Vec<_>>()
    };
    assert_eq!(counts(&small), counts(&big));
    assert_eq!(builds(&small), builds(&big));
    // At most one text a row of the window: 69 rows of log at 70.
    assert!(builds(&big).iter().all(|&b| b <= 69), "{big:?}");
    // With wrap off, a row up builds the one line it brings in, and the
    // steps sideways nothing: the texts drawn are kept.
    assert!(builds(&big)[9..12].iter().all(|&b| b <= 1), "{big:?}");
    // Each frame measures at most the window and the paging edges' five
    // screens each way, in entries: a few hundred at 70 rows, not 20,000.
    assert!(counts(&big).iter().all(|&c| c < 2_000), "{big:?}");
    // A new layout at the bottom measures the lines in view (a 5,000-
    // character record is 21 rows at 240 columns: four fill 70 rows), not
    // the paging edge's five screens: the entries alone tell that the
    // oldest is far.
    assert!(counts(&big)[..2].iter().all(|&c| c <= 5), "{big:?}");
    let mut times: Vec<_> = big.iter().map(|(_, _, time)| *time).collect();
    eprintln!("frame cost at 240x70, 20,000 records, by key: {times:?}");
    times.sort();
    eprintln!(
        "frame cost at 240x70, 20,000 records: median {:?}, slowest {:?}",
        times[times.len() / 2],
        times[times.len() - 1]
    );
    // Home past the buffer: the frame its page brings, the log's first
    // 500 records at the first column, a window's worth of texts built.
    let (built, time) = home_page_cost();
    eprintln!("frame cost at 240x70, Home's page of 500 long records: {time:?}, {built} built");
    assert!(built <= 69, "{built}");
    // While a whole-log search reads, each frame does the same work, and
    // the header's count and progress cost a look at the matches.
    let searching = frame_costs_searching(20_000, true);
    assert_eq!(counts(&searching), counts(&big));
    assert_eq!(builds(&searching), builds(&big));
    let mut times: Vec<_> = searching.iter().map(|(_, _, time)| *time).collect();
    eprintln!("frame cost at 240x70, 20,000 records, searching, by key: {times:?}");
    times.sort();
    eprintln!(
        "frame cost at 240x70, 20,000 records, searching: median {:?}, slowest {:?}",
        times[times.len() / 2],
        times[times.len() - 1]
    );
    // Far to the right with wrap off (the follow-ups "Far-right wrap-off
    // frames are fast too" and "The viewer's far-right rebuild frame
    // meets 5 ms"), with colour off and on.
    far_right_holds("ascii", &"lorem ipsum ".repeat(420), &format());
    let truecolor = Format::new(
        TimeZone::UTC,
        Theme::resolve(&Palette::default(), Profile::TrueColor),
    );
    for (name, words) in far_records() {
        far_right_holds(name, &words, &truecolor);
    }
}

/// Heights are kept with their entries: a frame with nothing new measures
/// no log line, a followed line is measured once, and a new layout (`w`,
/// `t`, the width) measures again what the window needs.
#[test]
fn a_frame_measures_only_lines_it_hasnt_measured() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(200));
    let measured = |h: &mut Harness| h.viewer.measured.replace(0);
    measured(&mut h);
    h.draw();
    assert_eq!(measured(&mut h), 0);
    h.send(Event::Followed(vec![line(200, "new")]));
    assert_eq!(measured(&mut h), 1);
    h.key(KeyCode::Char('k'));
    assert_eq!(measured(&mut h), 0);
    h.key(KeyCode::Char('w'));
    assert!(measured(&mut h) > 0);
    h.key(KeyCode::Char('w'));
    measured(&mut h);
    h.key(KeyCode::Char('t'));
    assert!(measured(&mut h) > 0);
    h.key(KeyCode::Char('t'));
    measured(&mut h);
    h.terminal.backend_mut().resize(100, 70);
    h.draw();
    assert!(measured(&mut h) > 0);
}

/// The texts drawn are kept for the next frame: a frame with nothing new
/// builds none, a followed line only its own, the frame that turns wrap
/// off only those it didn't draw already, a step sideways none while
/// the texts reach that far; a change of the search, the theme or `t`
/// builds them all again.
#[test]
fn a_frame_builds_only_texts_it_hasnt_drawn() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(200));
    let built = |h: &mut Harness| h.viewer.built.replace(0);
    built(&mut h);
    h.draw();
    assert_eq!(built(&mut h), 0);
    // With wrap on, measured (a wrap of the whole text) and drawn.
    h.send(Event::Followed(vec![line(200, "new")]));
    assert_eq!(built(&mut h), 2);
    // Wrap off: the records' whole texts are kept, the others built.
    h.key(KeyCode::Char('w'));
    let switched = built(&mut h);
    assert!((60..=69).contains(&switched), "{switched}");
    h.draw();
    assert_eq!(built(&mut h), 0);
    // A screen more to the right is built: 30 steps of 8 columns.
    for _ in 0..30 {
        h.key(KeyCode::Right);
    }
    assert_eq!(built(&mut h), 0);
    h.key(KeyCode::Right);
    assert!(built(&mut h) > 60);
    h.search("ipsum");
    assert!(built(&mut h) > 60);
    h.viewer.format.theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
    h.draw();
    assert!(built(&mut h) > 60);
    h.key(KeyCode::Char('t'));
    assert!(built(&mut h) > 60);
}

/// A frame drawn from the kept texts is the frame built afresh: every
/// cell and style, with colour, a search, wrap on and off and sideways.
#[test]
fn kept_texts_draw_what_new_ones_do() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let truecolor = Format::new(
        TimeZone::UTC,
        Theme::resolve(&Palette::default(), Profile::TrueColor),
    );
    let mut h = open_with(120, 30, limits, truecolor);
    let mut words = String::new();
    for i in 0..300 {
        let colour = i % 8;
        words.push_str("\x1b[3");
        words.push_str(&colour.to_string());
        words.push_str("mword");
        words.push_str(&i.to_string());
        words.push_str("\x1b[0m ");
    }
    let mut ls: Vec<Line> = (0..100)
        .map(|i| line(i, &format!("m{i} {words}")))
        .collect();
    ls.push(line(100, &format!("{}\n", "x".repeat(100)).repeat(60)));
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: false,
        more_newer: false,
    });
    let fresh = |h: &mut Harness| {
        let kept = h.terminal.backend().buffer().clone();
        h.viewer.drawn = Drawn::default();
        h.draw();
        assert_eq!(&kept, h.terminal.backend().buffer());
    };
    fresh(&mut h);
    h.search("word1");
    fresh(&mut h);
    h.key(KeyCode::Char('k'));
    fresh(&mut h);
    h.key(KeyCode::Char('w'));
    fresh(&mut h);
    for _ in 0..20 {
        h.key(KeyCode::Right);
        h.key(KeyCode::Char('k'));
        fresh(&mut h);
    }
    // A larger window needs more rows and columns than the texts kept.
    h.terminal.backend_mut().resize(200, 40);
    h.draw();
    fresh(&mut h);
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::PageUp);
    fresh(&mut h);
    // Wrap off, a record of 60 screen lines comes into view a row at a
    // time from below.
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::End);
    for _ in 0..3 {
        h.key(KeyCode::PageUp);
    }
    for _ in 0..40 {
        h.key(KeyCode::Char('j'));
        fresh(&mut h);
    }
}

/// The kept texts hold at most the viewer's budget of bytes between
/// them: past it, a frame keeps what fits and draws the rest as before,
/// built again the next frame.
#[test]
fn kept_texts_stay_within_their_bytes() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(200));
    h.key(KeyCode::Char('w'));
    let screen = h.terminal.backend().buffer().clone();
    let sizes: Vec<usize> = h.viewer.drawn.texts.iter().map(Built::size).collect();
    assert!(sizes.len() > 60, "{}", sizes.len());
    // Each text a line of text at least: its spans and contents.
    assert!(sizes.iter().all(|&size| size > 240), "{sizes:?}");
    let budget = sizes.iter().sum::<usize>() / 2;
    h.viewer.drawn_bytes = budget;
    h.viewer.drawn = Drawn::default();
    h.draw();
    let kept: Vec<usize> = h.viewer.drawn.texts.iter().map(Built::size).collect();
    assert!(!kept.is_empty() && kept.len() < sizes.len(), "{kept:?}");
    assert!(kept.iter().sum::<usize>() <= budget, "{kept:?}");
    assert_eq!(&screen, h.terminal.backend().buffer());
    h.viewer.built.set(0);
    h.draw();
    assert!(h.viewer.built.get() > 0);
    assert_eq!(&screen, h.terminal.backend().buffer());
}

/// A kept text's size counts its marks once a frame has cut it.
#[test]
fn a_kept_texts_size_counts_its_marks() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(200));
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::PageUp);
    h.key(KeyCode::Right);
    let kept = &h.viewer.drawn.texts;
    assert!(kept.len() > 60, "{}", kept.len());
    for built in kept {
        let marks = built.marks.as_ref().expect("cut at column 9");
        let bare = Built::new(built.id, built.text.clone(), built.rows, built.columns);
        assert!(marks.size() > 0);
        assert_eq!(built.size(), bare.size() + marks.size());
    }
}

/// With wrap off, a text built whole is kept for every column: steps
/// right past a screen and more build nothing, and draw what texts
/// built afresh do.
#[test]
fn a_whole_text_is_kept_for_every_column() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    let words = "lorem ipsum ".repeat(50);
    let ls: Vec<Line> = (0..400)
        .map(|i| line(i, &format!("m{i} {words}")))
        .collect();
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: false,
        more_newer: false,
    });
    h.key(KeyCode::Char('w'));
    // Records that come into view with wrap off are built for it.
    h.key(KeyCode::PageUp);
    h.viewer.built.set(0);
    for _ in 0..40 {
        h.key(KeyCode::Right);
    }
    assert_eq!(h.viewer.built.get(), 0);
    let kept = h.terminal.backend().buffer().clone();
    h.viewer.drawn = Drawn::default();
    h.draw();
    assert_eq!(&kept, h.terminal.backend().buffer());
}

/// A text's marks are made again for another window width: they count a
/// grapheme wider than the window as none.
#[test]
fn marks_follow_the_window_width() {
    let mut built = Built::new(0, Text::from("日本語".repeat(100)), usize::MAX, usize::MAX);
    let _ = built.window(0, 1, 8, 40);
    assert_eq!(built.marks.as_ref().map(Marks::width), Some(40));
    let _ = built.window(0, 1, 8, 40);
    assert_eq!(built.marks.as_ref().map(Marks::width), Some(40));
    let one: Vec<String> = built
        .window(0, 1, 8, 1)
        .0
        .lines
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(built.marks.as_ref().map(Marks::width), Some(1));
    // In one column no character fits: ratatui skips each and draws
    // nothing, and the cut, with marks for that width, is empty (with
    // the 40-column marks it would hold the characters from column 8).
    assert_eq!(one, [""]);
}

/// Far to the right a text is built from a little left of the window
/// through a little right of it: a few steps either way build nothing,
/// and more build each line in view again, from its kept places.
#[test]
fn far_right_steps_build_only_past_the_texts_kept() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(200));
    h.key(KeyCode::Char('w'));
    h.viewer.column = 2000;
    h.draw();
    let built = |h: &mut Harness, key: KeyCode, times: usize| {
        h.viewer.built.set(0);
        h.viewer.parsed.set(0);
        for _ in 0..times {
            h.key(key);
        }
        (h.viewer.built.get(), h.viewer.parsed.get())
    };
    // An eighth of a screen left (30 columns) and a quarter right.
    assert_eq!(built(&mut h, KeyCode::Left, 3), (0, 0));
    assert_eq!(built(&mut h, KeyCode::Right, 3 + 7), (0, 0));
    // One more step right builds the log lines in view again.
    let (again, parsed) = built(&mut h, KeyCode::Right, 1);
    assert!(again > 60, "{again}");
    assert!(parsed <= again * 600, "{parsed}");
}

/// With wrap off, the lines made ahead are the window's, a screen either
/// way, the buffer's two ends and a second screen either way, and with a
/// search the lines around the match before and after the window; a line
/// that fits in the window needs nothing.
#[test]
fn the_lines_made_ahead_are_those_the_next_keys_bring() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    let mut ls: Vec<Line> = (0..2_000)
        .map(|i| line(i, &format!("m{i} {}", "lorem ipsum ".repeat(420))))
        .collect();
    // One match, far above the window, and a short line in view.
    ls[300] = line(300, &format!("needle {}", "lorem ipsum ".repeat(420)));
    ls[1_000] = line(1_000, "short");
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: true,
        more_newer: false,
    });
    h.key(KeyCode::Char('w'));
    h.viewer.anchor = Anchor { item: 980, skip: 0 };
    h.viewer.follow = Follow::Off;
    h.draw();
    h.viewer.settle_ahead();
    let screen = 69;
    // Seven screens, the short line left out.
    assert_eq!(h.viewer.ahead.kept(), 7 * screen - 1);
    // The search counts its matches and jumps to the one before the
    // window; back to where it was.
    h.viewer.search("needle".to_owned());
    h.viewer.anchor = Anchor { item: 980, skip: 0 };
    h.draw();
    h.viewer.settle_ahead();
    // And the lines around the match: half a screen above it, a screen
    // from it, none of them in the band already.
    assert_eq!(h.viewer.ahead.kept(), 7 * screen - 1 + screen / 2 + screen);
}

/// A jump sideways from the first column, the rows the same, builds the
/// lines in view from the places made ahead for them, not from their
/// starts.
#[test]
fn a_jump_sideways_uses_places_made_ahead() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(2_000));
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::PageUp);
    h.viewer.settle_ahead();
    h.viewer.built.set(0);
    h.viewer.parsed.set(0);
    h.viewer.column = 4000;
    h.draw();
    let (built, parsed) = (h.viewer.built.get(), h.viewer.parsed.get());
    assert!(built > 60, "{built}");
    assert!(parsed <= built * 1000, "{parsed} bytes for {built} lines");
}

/// Far to the right, lines seen for the first time are ready too: their
/// places and texts are made ahead, off the viewer's thread, for the
/// screens around the window and the buffer's ends, so a page up, Home or
/// End there builds nothing; with wrap on nothing is asked for.
#[test]
fn far_right_first_sight_uses_places_made_ahead() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let mut h = open(240, 70, limits);
    h.send(long_tail(2_000));
    h.draw();
    assert_eq!(h.viewer.ahead.pending(), 0, "wrap on: nothing asked for");
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::PageUp);
    h.viewer.column = 4000;
    h.draw();
    for key in [
        KeyCode::PageUp,
        KeyCode::PageUp,
        KeyCode::Home,
        KeyCode::End,
    ] {
        h.viewer.settle_ahead();
        h.viewer.built.set(0);
        h.viewer.parsed.set(0);
        h.key(key);
        // Their texts were made ahead for this window: nothing built.
        let (built, parsed) = (h.viewer.built.get(), h.viewer.parsed.get());
        assert_eq!((built, parsed), (0, 0), "{key:?}");
    }
    assert!(h.viewer.ahead.bytes() <= crate::ahead::AHEAD_BYTES);
    // A page up from the middle parses from their start only the lines
    // new to the band (a screen's worth), not those it drew or had.
    h.key(KeyCode::PageUp);
    h.key(KeyCode::PageUp);
    h.viewer.settle_ahead();
    let fresh = h.viewer.ahead.fresh();
    h.key(KeyCode::PageUp);
    h.viewer.settle_ahead();
    let new = h.viewer.ahead.fresh() - fresh;
    assert!(
        (60..=69).contains(&new),
        "{new} lines parsed from their start"
    );
}

/// Whether the window draws as ratatui's `Paragraph::scroll` draws each
/// whole text in view (`step` names the frame on a mismatch). The frame
/// is as ratatui composed it: the test backend's own buffer keeps the
/// cell after a wide character from an earlier frame, which a terminal
/// overwrites when it draws the character.
fn draws_as_paragraph_scroll(h: &mut Harness, step: &str) {
    use ratatui::widgets::Widget as _;
    let Harness {
        viewer, terminal, ..
    } = h;
    let screen = terminal
        .draw(|frame| viewer.draw(frame))
        .expect("drawn")
        .buffer
        .clone();
    let top = viewer.top();
    let rows = u16::try_from(viewer.height).expect("a small window");
    let width = viewer.width;
    let area = Rect::new(0, 0, width, 1);
    for y in 0..rows {
        let item = top.item + isize::try_from(y).expect("a row");
        let mut expected = ratatui::buffer::Buffer::empty(area);
        Paragraph::new(viewer.item_text(item))
            .scroll((0, viewer.column))
            .render(area, &mut expected);
        for x in 0..width {
            assert_eq!(
                screen[(x, y + 1)],
                expected[(x, 0)],
                "{step}: at {x},{y}, column {}",
                viewer.column
            );
        }
    }
}

/// Far to the right with wrap off, the window draws as ratatui's
/// `Paragraph::scroll` draws each whole text in view, through steps both
/// ways, `t`, a search, a resize and a page up, the texts built from
/// their lines' kept resume places and from nothing.
#[test]
fn far_right_texts_draw_as_paragraph_scroll() {
    let limits = Limits {
        entries: 20_000,
        bytes: usize::MAX,
    };
    let truecolor = Format::new(
        TimeZone::UTC,
        Theme::resolve(&Palette::default(), Profile::TrueColor),
    );
    let mut h = open_with(100, 16, limits, truecolor);
    let codes = [
        "\x1b[1;31m",
        "\x1b[22m",
        "\x1b[38;5;99m",
        "\x1b[4;44m",
        "\x1b[0m",
        "日本 ",
    ];
    let ls: Vec<Line> = (0..300)
        .map(|i| {
            let words: String = (0..400)
                .map(|w| format!("{}word{w} ", codes[(w + i) % codes.len()]))
                .collect::<Vec<_>>()
                .concat();
            Line {
                time: Some("2026-11-13T09:04:05Z".parse().expect("a time")),
                origin: b"app[7]".to_vec(),
                ..line(i as u64, &words)
            }
        })
        .collect();
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: false,
        more_newer: false,
    });
    h.key(KeyCode::Char('w'));
    h.viewer.column = 2000;
    h.draw();
    draws_as_paragraph_scroll(&mut h, "from nothing");
    let mut steps = vec![("right", KeyCode::Right); 12];
    steps.extend(vec![("left", KeyCode::Left); 30]);
    steps.extend([
        ("t", KeyCode::Char('t')),
        ("t", KeyCode::Char('t')),
        ("up", KeyCode::Char('k')),
    ]);
    for (step, key) in steps {
        h.key(key);
        draws_as_paragraph_scroll(&mut h, step);
    }
    h.viewer.query = "ord2".to_owned();
    h.draw();
    draws_as_paragraph_scroll(&mut h, "search");
    h.terminal.backend_mut().resize(90, 16);
    h.draw();
    draws_as_paragraph_scroll(&mut h, "resize");
    h.key(KeyCode::PageUp);
    draws_as_paragraph_scroll(&mut h, "page up");
    // Lines brought in from texts the worker made ahead (nothing built in
    // the frame), with the search on and at the new width, then with `t`
    // off and no search.
    for stage in ["search", "t off"] {
        if stage == "t off" {
            h.viewer.query.clear();
            h.key(KeyCode::Char('t'));
        }
        for (step, key) in [
            ("page up", KeyCode::PageUp),
            ("Home", KeyCode::Home),
            ("End", KeyCode::End),
        ] {
            h.viewer.settle_ahead();
            h.viewer.built.set(0);
            h.key(key);
            assert_eq!(h.viewer.built.get(), 0, "{stage}, {step}: made ahead");
            draws_as_paragraph_scroll(&mut h, &format!("{stage}, {step} from texts made ahead"));
        }
    }
}

/// With wrap off and scrolled sideways, a tall record partly above the
/// window draws as ratatui's `Paragraph::scroll` draws its whole text.
#[test]
fn a_tall_record_scrolled_sideways_draws_as_paragraph_scroll() {
    use ratatui::widgets::Widget as _;
    let mut h = open(40, 12, Limits::default());
    let row = "abcdefghij".repeat(10);
    let tall = (0..60).fold(String::new(), |mut tall, i| {
        use std::fmt::Write as _;
        let _ = writeln!(tall, "r{i:02} {row}");
        tall
    });
    let mut ls: Vec<Line> = (0..5).map(|i| line(i, &format!("short {i}"))).collect();
    ls.push(line(5, &tall));
    ls.extend((6..9).map(|i| line(i, &format!("short {i}"))));
    h.send(Event::Page {
        kind: PageKind::Tail,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: false,
        more_newer: false,
    });
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::Home);
    for _ in 0..3 {
        h.key(KeyCode::Right);
    }
    for _ in 0..9 {
        h.key(KeyCode::Char('j'));
    }
    let top = h.viewer.top();
    assert_eq!(top.item, 5, "{top:?}");
    assert!(top.skip > 0, "{top:?}");
    let rows = u16::try_from(h.viewer.height).expect("a small window");
    let area = Rect::new(0, 0, 40, rows);
    let mut expected = ratatui::buffer::Buffer::empty(area);
    Paragraph::new(h.viewer.item_text(top.item))
        .scroll((u16::try_from(top.skip).expect("a few rows"), 24))
        .render(area, &mut expected);
    let screen = h.terminal.backend().buffer();
    for y in 0..rows {
        for x in 0..40 {
            assert_eq!(screen[(x, y + 1)], expected[(x, y)], "at {x},{y}");
        }
    }
}

/// A paste into the search line is one input, its newlines spaces, a
/// last newline dropped: the line stays open for Enter. Outside the
/// line, a paste does nothing.
#[test]
fn a_paste_goes_into_the_search_line_whole() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(0..50, false));
    assert_eq!(h.viewer.input(Input::Paste("q".into())), Flow::Unchanged);
    h.key(KeyCode::Char('/'));
    assert_eq!(h.viewer.input(Input::Paste("m4\r\n".into())), Flow::Redraw);
    let line = h.viewer.searching.as_ref().expect("still open");
    assert_eq!(line.lines(), ["m4"]);
    h.key(KeyCode::Enter);
    assert!(h.header().contains("/m4 11/11"), "{}", h.header());
}

// --- the whole-log search (tui spec, "Whole-log search, save and print") ---

/// The searches the viewer asked for since the last look, and whether it
/// asked to stop one.
fn searches(requests: &[Request]) -> (Vec<Search>, usize) {
    let asked = requests
        .iter()
        .filter_map(|r| match r {
            Request::Search(search) => Some(search.clone()),
            _ => None,
        })
        .collect();
    let stops = requests
        .iter()
        .filter(|r| matches!(r, Request::StopSearch))
        .count();
    (asked, stops)
}

/// The jumps the viewer asked for since the last look (paging requests
/// around them left out).
fn jumps(requests: &[Request]) -> Vec<Request> {
    requests
        .iter()
        .filter(|r| matches!(r, Request::Around(_)))
        .cloned()
        .collect()
}

fn found_at(ns: &[u64]) -> Vec<Found> {
    ns.iter()
        .map(|&n| Found {
            position: position(n),
            time: None,
            count: 1,
        })
        .collect()
}

/// A step of the search `id`'s `side`.
fn step(id: u64, side: Side, order: Order, ns: &[u64], end: Option<SearchEnd>) -> Event {
    Event::Found {
        search: id,
        side,
        order,
        found: found_at(ns),
        reached: None,
        end,
    }
}

/// A jump's page around `at`: lines `range`, `at`'s message `hit`.
fn around(range: std::ops::Range<u64>, at: u64, hit: &str) -> Event {
    let ls: Vec<Line> = range
        .map(|n| {
            if n == at {
                line(n, hit)
            } else {
                line(n, &format!("m{n}"))
            }
        })
        .collect();
    Event::Page {
        kind: PageKind::Around,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: true,
        more_newer: true,
    }
}

/// Enter starts the whole-log search: the older side from the buffer's
/// oldest edge, the newer side not while following; the source counts
/// with the screen's own match. Esc and a new query stop it.
#[test]
fn a_search_asks_the_source_to_read_the_rest_of_the_log() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.search("m4");
    let (asked, stops) = searches(&h.requests());
    assert_eq!(stops, 0);
    let [search] = &asked[..] else {
        panic!("one search: {asked:?}")
    };
    assert_eq!(search.older, Some(position(40)));
    assert_eq!(search.newer, None, "following: nothing newer to read");
    assert_eq!(search.matcher.count(&line(3, "m4 m4")), 2);
    assert!(
        h.header().contains("/m4 10/10+  searching…"),
        "{}",
        h.header()
    );
    h.search("m45");
    let (asked, stops) = searches(&h.requests());
    assert_eq!((asked.len(), stops), (1, 1));
    assert_eq!(asked[0].id, search.id + 1);
    // A step of the first search is dropped.
    h.send(step(
        search.id,
        Side::Older,
        Order::NearestFirst,
        &[4],
        None,
    ));
    assert!(h.header().contains("/m45 1/1+"), "{}", h.header());
    h.key(KeyCode::Esc);
    assert_eq!(searches(&h.requests()).1, 1);
    assert!(!h.header().contains("/m45"), "{}", h.header());
}

/// Paged away from the end, the newer side is read too.
#[test]
fn a_search_off_the_end_reads_the_newer_side() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.key(KeyCode::Char('f'));
    // Paged away from the end, as after a jump to a match.
    h.viewer.around = Some((position(25), 0));
    h.send(around(20..30, 25, "x"));
    h.requests();
    h.search("x");
    let (asked, _) = searches(&h.requests());
    assert_eq!(asked[0].older, Some(position(20)));
    assert_eq!(asked[0].newer, Some(position(29)));
}

/// `N` past the buffer: the older side's nearest match, its
/// surroundings asked of the source, the buffer starting again there,
/// not following, the match current and numbered among the log's.
#[test]
fn n_past_the_buffer_loads_the_matchs_surroundings() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.search("m45");
    let id = searches(&h.requests()).0[0].id;
    h.send(step(id, Side::Older, Order::NearestFirst, &[20, 10], None));
    assert!(h.header().contains("/m45 3/3+"), "{}", h.header());
    h.key(KeyCode::Char('N'));
    assert_eq!(jumps(&h.requests()), [Request::Around(position(20))]);
    h.send(around(15..25, 20, "the m45 before"));
    assert!(!h.viewer.following());
    assert_eq!(h.viewer.current, Some((5, 0)));
    assert!(h.header().contains("/m45 2/3+"), "{}", h.header());
    assert!(h.log()[0].contains("the m45 before"), "{:?}", h.log());
    // And on to the next older one.
    h.key(KeyCode::Char('N'));
    assert_eq!(jumps(&h.requests()), [Request::Around(position(10))]);
    h.send(around(5..15, 10, "m45 first"));
    assert!(h.header().contains("/m45 1/3+"), "{}", h.header());
    // Back the other way: the match after this buffer.
    h.key(KeyCode::Char('n'));
    assert_eq!(jumps(&h.requests()), [Request::Around(position(20))]);
}

/// A match not read yet is waited for: `N` with the older side sending
/// farthest first waits for its end; another key gives up the wait; past
/// the last match with the search done, nothing happens.
#[test]
fn a_jump_waits_for_a_match_not_read_yet() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.search("m45");
    let id = searches(&h.requests()).0[0].id;
    h.send(step(id, Side::Older, Order::FarthestFirst, &[5, 10], None));
    h.key(KeyCode::Char('N'));
    assert_eq!(jumps(&h.requests()), [], "the nearest isn't known yet");
    h.send(step(
        id,
        Side::Older,
        Order::FarthestFirst,
        &[20],
        Some(SearchEnd::Done),
    ));
    assert_eq!(jumps(&h.requests()), [Request::Around(position(20))]);

    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.search("m45");
    let id = searches(&h.requests()).0[0].id;
    h.key(KeyCode::Char('N'));
    h.key(KeyCode::Char('k'));
    h.send(step(id, Side::Older, Order::NearestFirst, &[20], None));
    assert_eq!(jumps(&h.requests()), [], "the wait was given up");
    h.send(step(
        id,
        Side::Older,
        Order::NearestFirst,
        &[],
        Some(SearchEnd::Done),
    ));
    h.key(KeyCode::Char('n'));
    assert_eq!(jumps(&h.requests()), [], "past the last match");
}

/// The header's forms: how far back while reading, the cap, a failure.
#[test]
fn the_header_shows_how_the_search_stands() {
    let mut h = open(90, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.search("m45");
    let id = searches(&h.requests()).0[0].id;
    let two_days = Timestamp::now() - jiff::SignedDuration::from_hours(49);
    h.send(Event::Found {
        search: id,
        side: Side::Older,
        order: Order::NearestFirst,
        found: found_at(&[20]),
        reached: Some(two_days),
        end: None,
    });
    assert!(
        h.header().contains("/m45 2/2+  searching… 2 days back"),
        "{}",
        h.header()
    );
    h.send(step(
        id,
        Side::Older,
        Order::NearestFirst,
        &[],
        Some(SearchEnd::Capped),
    ));
    assert!(
        h.header().contains("/m45 2/2  stopped at 10,000 matches"),
        "{}",
        h.header()
    );
    h.search("m44");
    let id = searches(&h.requests()).0[0].id;
    h.send(step(
        id,
        Side::Older,
        Order::NearestFirst,
        &[],
        Some(SearchEnd::Failed("x".into())),
    ));
    assert!(
        h.header().contains("/m44 1/1  search stopped: read error"),
        "{}",
        h.header()
    );
    h.search("zz");
    let id = searches(&h.requests()).0[0].id;
    assert!(h.header().contains("/zz  searching…"), "{}", h.header());
    h.send(step(
        id,
        Side::Older,
        Order::NearestFirst,
        &[],
        Some(SearchEnd::Done),
    ));
    assert!(h.header().contains("/zz no matches"), "{}", h.header());
}

/// While following, the follow's new matches join the count.
#[test]
fn the_follows_matches_join_the_count() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.search("m4");
    h.search_answered(&[4]);
    assert!(h.header().contains("/m4 11/11"), "{}", h.header());
    h.send(Event::Followed(vec![
        line(50, "m4 again"),
        line(51, "none"),
    ]));
    assert!(h.header().contains("/12"), "{}", h.header());
}

/// A side that couldn't read on says why in a notice row, as a page
/// does, and the header says the search stopped.
#[test]
fn a_failed_side_says_why() {
    let mut h = open(100, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.search("m45");
    let id = searches(&h.requests()).0[0].id;
    h.send(step(
        id,
        Side::Older,
        Order::FarthestFirst,
        &[],
        Some(SearchEnd::Failed("the daemon went away".into())),
    ));
    let screen = h.screen().join("\n");
    assert!(
        screen.contains("couldn't read the log: the daemon went away"),
        "{screen}"
    );
    assert!(
        h.header().contains("search stopped: read error"),
        "{}",
        h.header()
    );
}

/// Back at the end from a search that read the newer side, the search
/// starts again as one while following: older only.
#[test]
fn back_at_the_end_a_search_reads_older_only() {
    let mut h = open(60, 8, Limits::default());
    h.requests();
    h.send(tail(40..50, true));
    h.key(KeyCode::Char('f'));
    // Paged away from the end, as after a jump to a match.
    h.viewer.around = Some((position(25), 0));
    h.send(around(20..30, 25, "x"));
    h.requests();
    h.search("x");
    let (asked, _) = searches(&h.requests());
    assert!(asked[0].newer.is_some());
    h.send(tail(40..50, true));
    let (asked, stops) = searches(&h.requests());
    assert_eq!(stops, 1);
    let [search] = &asked[..] else {
        panic!("one search: {asked:?}")
    };
    assert_eq!(search.newer, None);
    assert_eq!(search.older, Some(position(40)));
}

// --- Home and End (tui spec, "Home and End: the log's ends") ---

fn head_page(range: std::ops::Range<u64>, more_newer: bool) -> Event {
    let ls = lines(range);
    Event::Page {
        kind: PageKind::Head,
        oldest: ls.first().map(|l| l.position.clone()),
        newest: ls.last().map(|l| l.position.clone()),
        lines: ls,
        more_older: false,
        more_newer,
    }
}

/// With the log's first line loaded, Home only moves to the top.
#[test]
fn home_with_the_start_loaded_moves_to_the_top() {
    let mut h = open(60, 8, Limits::default());
    h.send(tail(1..20, false));
    h.requests();
    h.key(KeyCode::Home);
    assert!(
        !h.requests().contains(&Request::Head),
        "the start is loaded"
    );
    assert_eq!(h.log()[0], "--- start of log ---");
    assert!(h.header().contains("at the start"), "{}", h.header());
}

/// Home past the buffer: the source's first page, the buffer started
/// again there, not following, the start marker at the top; End reads
/// the tail again and follows.
#[test]
fn home_loads_the_logs_first_page_and_end_goes_back() {
    let mut h = open(70, 8, Limits::default());
    h.send(tail(40..50, true));
    h.requests();
    h.key(KeyCode::Char('g'));
    assert!(h.requests().contains(&Request::Head));
    assert!(h.header().contains("going to the start…"), "{}", h.header());
    assert!(!h.viewer.following());
    // One ask at a time.
    h.key(KeyCode::Home);
    assert!(!h.requests().contains(&Request::Head));
    h.send(head_page(0..10, true));
    assert_eq!(h.log()[0], "--- start of log ---");
    assert_eq!(h.log()[1], "m0");
    assert!(h.header().contains("at the start"), "{}", h.header());
    assert!(!h.header().contains("going to"), "{}", h.header());
    assert!(!h.viewer.following());
    h.key(KeyCode::End);
    let requests = h.requests();
    assert!(requests.contains(&Request::Tail), "{requests:?}");
    assert!(requests.contains(&Request::Follow(true)), "{requests:?}");
}

/// The start's header names the first line's time, as the lines show it.
#[test]
fn the_start_header_names_the_first_lines_time() {
    let mut h = open(100, 8, Limits::default());
    h.send(tail(40..50, true));
    h.key(KeyCode::Home);
    let mut first = line(0, "m0");
    first.time = Some("2026-11-13T09:04:05Z".parse().expect("a time"));
    h.send(Event::Page {
        kind: PageKind::Head,
        oldest: Some(position(0)),
        newest: Some(position(1)),
        lines: vec![first, line(1, "m1")],
        more_older: false,
        more_newer: false,
    });
    assert!(
        h.header()
            .contains("at the start · 2026-11-13T09:04:05+0000"),
        "{}",
        h.header()
    );
}

/// A first page that couldn't be read leaves the view, the ask given up.
#[test]
fn a_head_that_cant_be_read_gives_up_the_ask() {
    let mut h = open(70, 8, Limits::default());
    h.send(tail(40..50, true));
    h.key(KeyCode::Home);
    h.send(Event::Notice(Notice::ReadFailed("gone".into())));
    assert!(!h.header().contains("going to"), "{}", h.header());
    h.requests();
    h.key(KeyCode::Home);
    assert!(h.requests().contains(&Request::Head));
}

/// End while Home's page is read gives up Home: the page that comes
/// after is dropped, and the view follows the tail.
#[test]
fn end_while_going_to_the_start_gives_it_up() {
    let mut h = open(70, 8, Limits::default());
    h.send(tail(40..50, true));
    h.key(KeyCode::Home);
    h.key(KeyCode::End);
    h.send(head_page(0..10, true));
    assert!(h.viewer.following(), "{}", h.header());
    assert!(!h.log().iter().any(|row| row == "m0"), "{:?}", h.log());
    assert!(!h.header().contains("going to"), "{}", h.header());
}

/// No paging is asked for while a jump's page is awaited: an older or
/// newer page of the old place would land in the new buffer.
#[test]
fn no_paging_while_going_to_the_start() {
    let mut h = open(70, 8, Limits::default());
    h.send(tail(40..50, true));
    h.key(KeyCode::Home);
    h.requests();
    // The first frame's page back comes in while Home's page is read.
    h.send(older(30..40, true));
    h.key(KeyCode::PageUp);
    let requests = h.requests();
    assert!(
        !requests
            .iter()
            .any(|r| matches!(r, Request::Older(_) | Request::Newer(_))),
        "{requests:?}"
    );
}

/// After Home, with the buffer at the log's start and none of the
/// search's matches in it, `n` goes to the log's first match, once the
/// older side has read to the start, and waits while it reads.
#[test]
fn n_after_home_goes_to_the_logs_first_match() {
    let mut h = open(70, 8, Limits::default());
    h.send(tail(40..50, true));
    h.search("m45");
    let id = searches(&h.requests()).0[0].id;
    h.send(step(id, Side::Older, Order::NearestFirst, &[30, 20], None));
    h.key(KeyCode::Home);
    h.send(head_page(0..10, true));
    h.requests();
    h.key(KeyCode::Char('n'));
    assert_eq!(jumps(&h.requests()), [], "the oldest isn't read yet");
    h.send(step(
        id,
        Side::Older,
        Order::NearestFirst,
        &[15],
        Some(SearchEnd::Done),
    ));
    assert_eq!(jumps(&h.requests()), [Request::Around(position(15))]);
}

/// Following at the bottom of a short log, its start in view, the header
/// adds nothing.
#[test]
fn no_start_header_while_following_at_the_bottom() {
    let mut h = open(70, 8, Limits::default());
    h.send(tail(1..3, false));
    assert!(h.viewer.following());
    assert!(!h.header().contains("at the start"), "{}", h.header());
}

/// No paging is asked for while a match's page is awaited either.
#[test]
fn no_paging_while_a_matchs_page_is_awaited() {
    let mut h = open(70, 8, Limits::default());
    h.send(tail(40..50, true));
    h.viewer.around = Some((position(5), 0));
    h.requests();
    h.send(older(30..40, true));
    h.key(KeyCode::PageUp);
    let requests = h.requests();
    assert!(
        !requests
            .iter()
            .any(|r| matches!(r, Request::Older(_) | Request::Newer(_))),
        "{requests:?}"
    );
}

/// The key map's keys (tui spec, "Keys: one map for every screen"): `b`
/// and Space page as `PgUp` and `PgDn`, `h` and `l` go sideways as ← and →
/// (wrap off), and `F` follows as End does.
#[test]
fn b_space_h_l_and_capital_f_are_the_key_maps() {
    let mut h = open(80, 6, Limits::default());
    h.requests();
    h.send(tail(0..30, false));
    h.key(KeyCode::Char('b'));
    let paged = h.log();
    h.key(KeyCode::Char(' '));
    h.key(KeyCode::PageUp);
    assert_eq!(h.log(), paged);
    // Wrap on: no sideways.
    assert_eq!(h.key(KeyCode::Char('l')), Flow::Unchanged);
    h.key(KeyCode::Char('w'));
    h.key(KeyCode::Char('l'));
    assert!(h.header().contains("col 9"), "{}", h.header());
    h.key(KeyCode::Char('h'));
    assert!(h.header().contains("col 1"), "{}", h.header());
    h.key(KeyCode::Char('F'));
    assert!(h.header().contains("following"), "{}", h.header());
    assert_eq!(
        h.log().last().map(String::as_str),
        Some("--- following ---")
    );
}

/// `?` (or F1) opens the keys overlay; any other key closes it and does
/// nothing else, so `q` doesn't quit and Esc doesn't go back; in the
/// search line `?` is typed and F1 still opens it.
#[test]
fn question_mark_opens_the_keys_and_any_key_closes_them() {
    let mut h = open(80, 23, Limits::default());
    h.requests();
    h.send(tail(0..5, false));
    assert_eq!(h.key(KeyCode::Char('?')), Flow::Redraw);
    assert!(h.screen().join("\n").contains("Keys: log viewer"));
    assert_eq!(h.key(KeyCode::Char('q')), Flow::Redraw);
    assert!(!h.screen().join("\n").contains("Keys:"));
    h.key(KeyCode::Char('?'));
    assert_eq!(h.key(KeyCode::Esc), Flow::Redraw);
    assert_eq!(h.viewer.leave(), None);
    h.key(KeyCode::Char('/'));
    h.key(KeyCode::Char('?'));
    assert!(h.screen().iter().any(|r| r.contains("Search: ?")));
    h.key(KeyCode::F(1));
    assert!(h.screen().join("\n").contains("Keys: log viewer"));
    assert_eq!(h.viewer.input(Input::Paste("x".into())), Flow::Unchanged);
}

#[test]
fn snapshot_keys_80x24() {
    let mut h = sample(80, 24);
    h.key(KeyCode::Char('?'));
    assert_snapshot!(h.terminal.backend());
}

#[test]
fn snapshot_keys_50x19() {
    let mut h = sample(50, 19);
    h.key(KeyCode::Char('?'));
    assert_snapshot!(h.terminal.backend());
}

/// The footer at the spec's widths (its mock-ups), with and without a
/// search in effect.
#[test]
fn the_footer_fits_120_80_and_50_columns() {
    for (width, plain, searched) in [
        (
            120,
            "Esc back · / search · f follow · w wrap · t time                                                         ? keys · q quit",
            "n/N match · Esc clear · f follow · w wrap · t time                                                       ? keys · q quit",
        ),
        (
            80,
            "Esc back · / search · f follow · w wrap · t time                 ? keys · q quit",
            "n/N match · Esc clear · f follow · w wrap · t time               ? keys · q quit",
        ),
        (
            50,
            "Esc back · / search · f follow     ? keys · q quit",
            "n/N match · Esc clear · f follow   ? keys · q quit",
        ),
    ] {
        let mut h = open(width, 6, Limits::default());
        h.requests();
        h.send(tail(0..5, false));
        assert_eq!(h.footer(), plain, "{width}");
        h.search("m1");
        assert_eq!(h.footer(), searched, "{width}");
    }
}

/// The header cuts the name with `…` first, so the state and the
/// search's count stay.
#[test]
fn the_header_cuts_the_name_before_the_state() {
    let mut h = open_with(50, 8, Limits::default(), format());
    h.viewer.name = "saltbox_managed_rclone_vfs_with_a_long_name".to_owned();
    h.requests();
    h.send(tail(0..5, false));
    h.search("m1");
    let header = h.header();
    assert!(
        header.ends_with("  service  following  /m1 1/1"),
        "{header}"
    );
    assert!(header.starts_with("saltbox_managed"), "{header}");
    assert!(header.contains('…'), "{header}");
    assert_eq!(Span::raw(header.as_str()).width(), 50, "{header}");
}

/// Every key the overlay lists does something here, and keys it doesn't
/// list do nothing (tui spec, "Keys", Testing): the save and print keys
/// are gone, and `u` and `d` no longer page.
#[test]
fn every_listed_key_acts_and_others_do_nothing() {
    for (name, event) in crate::hints::events(&KEYS, |_| true) {
        let mut h = open(40, 6, Limits::default());
        h.requests();
        h.send(tail(0..30, false));
        // Wrap off, so the arrows and `h` `l` go sideways.
        h.key(KeyCode::Char('w'));
        h.key(KeyCode::PageUp);
        assert_ne!(h.viewer.input(Input::Key(event)), Flow::Unchanged, "{name}");
    }
    for c in ['x', 'z', 'v', 'r', 's', 'p', 'u', 'd', 'c', 'a', 'e'] {
        let mut h = open(40, 6, Limits::default());
        h.requests();
        h.send(tail(0..30, false));
        assert_eq!(h.key(KeyCode::Char(c)), Flow::Unchanged, "{c}");
    }
}

/// Under the keys overlay the log takes nothing: the wheel doesn't
/// scroll it (nor pause the follow), and a key that moves the log only
/// closes the overlay.
#[test]
fn under_the_keys_the_wheel_and_keys_leave_the_log_alone() {
    let mut h = open(80, 23, Limits::default());
    h.requests();
    h.send(tail(0..60, false));
    h.key(KeyCode::PageUp);
    let before = h.log();
    h.key(KeyCode::Char('?'));
    h.mouse(MouseEventKind::ScrollUp);
    h.key(KeyCode::Char('j'));
    assert_eq!(h.log(), before);
    h.key(KeyCode::Char('?'));
    h.key(KeyCode::Char('G'));
    assert_eq!(h.log(), before);
    // Still paused: neither the wheel nor `G` reached the log.
    assert!(h.header().contains("following, paused"), "{}", h.header());
    // `F` while following stays following, as less's `F` (it never
    // turns the follow off; `f` does).
    h.key(KeyCode::Char('F'));
    h.key(KeyCode::Char('F'));
    assert!(h.header().contains("following"), "{}", h.header());
    assert!(!h.header().contains("paused"), "{}", h.header());
}
