//! The restore screen (ops spec, "`sb restore` (hidden)", decision 32):
//! the form's fields, checks and cancel; the live list's updates; the
//! viewer's menu, content shown as it is, cursor, focus and Continue;
//! the mouse. On ratatui's `TestBackend`.

use crossterm::event::{KeyModifiers, MouseEvent};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sb_cli_core::theme::{Palette, Profile};

use super::*;

const FILES: [&str; 3] = ["accounts.yml", "rclone.conf", "settings.yml"];

struct Harness {
    screen: Screen,
    answers: oneshot::Receiver<Answers>,
    updates: mpsc::UnboundedSender<Update>,
    _root: tempfile::TempDir,
}

fn harness(prefill: Prefill) -> Harness {
    let root = tempfile::tempdir().expect("root");
    let (answers_tx, answers) = oneshot::channel();
    let (updates, rx) = mpsc::unbounded_channel();
    let screen = Screen::new(
        Setup {
            files: &FILES,
            root: root.path().to_path_buf(),
            base: "http://fake".into(),
            theme: Theme::resolve(&Palette::default(), Profile::TrueColor),
        },
        prefill,
        answers_tx,
        rx,
    );
    Harness {
        screen,
        answers,
        updates,
        _root: root,
    }
}

fn key(h: &mut Harness, code: KeyCode) -> Flow {
    h.screen
        .input(Input::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn shift_tab(h: &mut Harness) -> Flow {
    h.screen.input(Input::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
    )))
}

fn typed(h: &mut Harness, text: &str) {
    for c in text.chars() {
        key(h, KeyCode::Char(c));
    }
}

fn drawn(h: &mut Harness, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
    terminal.draw(|f| h.screen.draw(f)).expect("draw");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

fn screen_text(h: &mut Harness) -> String {
    drawn(h, 100, 30).join("\n")
}

fn click(h: &mut Harness, column: u16, row: u16) -> Flow {
    h.screen.input(Input::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

fn wheel(h: &mut Harness, kind: MouseEventKind, column: u16, row: u16) -> Flow {
    h.screen.input(Input::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

/// Filled and submitted: the answers go to the fetch, the list shows,
/// the fields are emptied.
fn submit(h: &mut Harness, user: &str, password: &str) {
    typed(h, user);
    key(h, KeyCode::Tab);
    typed(h, password);
    key(h, KeyCode::Tab);
    typed(h, password);
    key(h, KeyCode::Enter);
}

#[test]
fn the_form_sends_the_answers_and_shows_the_list() {
    let mut h = harness(Prefill::default());
    submit(&mut h, "someone", "secret-pass");
    let answers = h.answers.try_recv().expect("answers sent");
    assert_eq!(answers.user.as_str(), "someone");
    assert_eq!(answers.password.as_slice(), b"secret-pass");
    assert_eq!(h.screen.mode, Mode::Run);
    assert!(h.screen.started);
    assert!(
        h.screen
            .fields
            .iter()
            .all(|f| f.lines().join("").is_empty())
    );
    let text = screen_text(&mut h);
    assert!(text.contains("Fetching files from http://fake"), "{text}");
    assert!(text.contains("accounts.yml         waiting"), "{text}");
}

/// Enter moves to the next field and submits from the last; Tab and
/// Shift-Tab, Down and Up move round.
#[test]
fn the_keys_move_between_the_fields() {
    let mut h = harness(Prefill::default());
    assert_eq!(h.screen.focus, 0);
    key(&mut h, KeyCode::Enter);
    assert_eq!(h.screen.focus, 1);
    key(&mut h, KeyCode::Down);
    assert_eq!(h.screen.focus, 2);
    key(&mut h, KeyCode::Tab);
    assert_eq!(h.screen.focus, 0);
    shift_tab(&mut h);
    assert_eq!(h.screen.focus, 2);
    key(&mut h, KeyCode::Up);
    assert_eq!(h.screen.focus, 1);
}

/// An empty user name, an empty password or two that differ show in the
/// form, and nothing starts.
#[test]
fn the_form_refuses_what_wont_do() {
    let mut h = harness(Prefill::default());
    key(&mut h, KeyCode::Tab);
    typed(&mut h, "pw");
    key(&mut h, KeyCode::Tab);
    typed(&mut h, "pw");
    key(&mut h, KeyCode::Enter);
    assert!(screen_text(&mut h).contains("Enter the user name"));
    assert_eq!(h.screen.mode, Mode::Form);
    let mut h = harness(Prefill::default());
    submit(&mut h, "someone", "");
    assert!(screen_text(&mut h).contains(MISMATCH));
    let mut h = harness(Prefill::default());
    typed(&mut h, "someone");
    key(&mut h, KeyCode::Tab);
    typed(&mut h, "one");
    key(&mut h, KeyCode::Tab);
    typed(&mut h, "two");
    key(&mut h, KeyCode::Enter);
    assert!(screen_text(&mut h).contains(MISMATCH));
    assert_eq!(h.screen.mode, Mode::Form);
    assert!(h.answers.try_recv().is_err(), "nothing started");
    assert!(!h.screen.started);
}

/// The passwords are masked in the form; the user name shows.
#[test]
fn the_passwords_are_masked_and_the_user_shows() {
    let mut h = harness(Prefill::default());
    typed(&mut h, "someone");
    key(&mut h, KeyCode::Tab);
    typed(&mut h, "hunter2");
    let text = screen_text(&mut h);
    assert!(text.contains("someone"), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
    assert!(text.contains("•••••••"), "{text}");
}

/// Ctrl-M and a typed `\n` move on, never a new line in a field; the
/// field's own keys edit it (Backspace).
#[test]
fn the_fields_stay_one_line() {
    let mut h = harness(Prefill::default());
    typed(&mut h, "someonex");
    key(&mut h, KeyCode::Backspace);
    h.screen.input(Input::Key(KeyEvent::new(
        KeyCode::Char('m'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(h.screen.focus, 1);
    key(&mut h, KeyCode::Char('\n'));
    assert_eq!(h.screen.focus, 2);
    assert_eq!(h.screen.fields[0].lines(), ["someone"]);
    assert!(h.screen.fields.iter().all(|f| f.lines().len() == 1));
}

#[test]
fn esc_in_the_form_cancels() {
    let mut h = harness(Prefill::default());
    assert_eq!(key(&mut h, KeyCode::Esc), Flow::Quit);
    assert!(h.screen.cancelled);
    assert!(!h.screen.started);
}

/// A given user fills its field and the password is asked; both given
/// skip the form.
#[test]
fn the_flags_fill_the_form() {
    let mut h = harness(Prefill {
        user: Some("someone".into()),
        password: None,
    });
    assert_eq!(h.screen.mode, Mode::Form);
    assert_eq!(h.screen.focus, 1);
    assert!(screen_text(&mut h).contains("someone"));
    let mut h = harness(Prefill {
        user: Some("someone".into()),
        password: Some(Zeroizing::new(b"pw".to_vec())),
    });
    assert_eq!(h.screen.mode, Mode::Run);
    let answers = h.answers.try_recv().expect("started");
    assert_eq!(answers.password.as_slice(), b"pw");
    // The password alone: both its fields filled, the user asked.
    let mut h = harness(Prefill {
        user: None,
        password: Some(Zeroizing::new(b"pw".to_vec())),
    });
    assert_eq!(h.screen.mode, Mode::Form);
    assert_eq!(h.screen.focus, 0);
    typed(&mut h, "someone");
    key(&mut h, KeyCode::Enter);
    key(&mut h, KeyCode::Enter);
    key(&mut h, KeyCode::Enter);
    assert_eq!(
        h.answers.try_recv().expect("sent").password.as_slice(),
        b"pw"
    );
}

/// A password file's bytes are sent as read, even ones a field can't
/// hold, unless a password field is edited; then the typed one is.
#[test]
fn a_given_password_is_sent_as_read() {
    let raw = b"a\nb\xff".to_vec();
    let mut h = harness(Prefill {
        user: None,
        password: Some(Zeroizing::new(raw.clone())),
    });
    typed(&mut h, "someone");
    key(&mut h, KeyCode::Enter);
    // Moving through the fields edits nothing.
    key(&mut h, KeyCode::Left);
    key(&mut h, KeyCode::Enter);
    key(&mut h, KeyCode::Enter);
    assert_eq!(h.answers.try_recv().expect("sent").password.as_slice(), raw);
    let mut h = harness(Prefill {
        user: Some("someone".into()),
        password: Some(Zeroizing::new(Vec::new())),
    });
    assert_eq!(h.screen.mode, Mode::Form, "an empty password is asked");
    for field in 1..3 {
        h.screen.focus = field;
        typed(&mut h, "new");
    }
    key(&mut h, KeyCode::Enter);
    assert_eq!(
        h.answers.try_recv().expect("sent").password.as_slice(),
        b"new"
    );
    let h = harness(Prefill {
        user: Some("someone".into()),
        password: Some(Zeroizing::new(b"old".to_vec())),
    });
    assert_eq!(h.screen.mode, Mode::Run, "both given");
    assert!(
        h.screen
            .fields
            .iter()
            .all(|f| f.lines().join("").is_empty()),
        "a skipped form holds nothing"
    );
}

/// Updates still queued when the screen closes are taken: a file placed
/// in the same tick as Ctrl-C is listed.
#[test]
fn queued_updates_reach_the_closed_list() {
    let mut h = harness(Prefill::default());
    submit(&mut h, "someone", "pw");
    h.updates
        .send(Update::Finished(0, Status::Done))
        .expect("send");
    let closed = h.screen.closed(Ending::Interrupted);
    assert_eq!(closed.rows[0].status, Status::Done);
    assert!(closed.started && !closed.cancelled);
    assert_eq!(closed.ending, Ending::Interrupted);
}

/// The run ended with these statuses for [`FILES`]: the viewer.
fn ended(h: &mut Harness, statuses: [Status; 3]) {
    for (i, status) in statuses.into_iter().enumerate() {
        h.updates.send(Update::Finished(i, status)).expect("send");
    }
    h.updates
        .send(Update::Summary("Restore process completed.".into()))
        .expect("send");
    h.screen.wake();
}

fn place(h: &Harness, name: &str, text: &str) {
    std::fs::write(h.screen.root.join(name), text).expect("placed");
}

/// The fetch's updates change the rows while the run lasts, when keys do
/// nothing; the summary opens the viewer on the first restored file.
#[test]
fn updates_drive_the_list_then_the_viewer() {
    let mut h = harness(Prefill::default());
    place(&h, "settings.yml", "shell: bash\n");
    submit(&mut h, "someone", "pw");
    assert_eq!(key(&mut h, KeyCode::Char('q')), Flow::Unchanged, "running");
    assert_eq!(key(&mut h, KeyCode::Char('c')), Flow::Unchanged, "running");
    assert!(screen_text(&mut h).contains("Ctrl-C stop"));
    h.updates.send(Update::Fetching(0)).expect("send");
    assert_eq!(h.screen.wake(), Flow::Redraw);
    assert!(screen_text(&mut h).contains("accounts.yml         fetching…"));
    h.updates
        .send(Update::Finished(
            0,
            Status::Failed("Decryption failed".into()),
        ))
        .expect("send");
    h.screen.wake();
    let text = screen_text(&mut h);
    assert!(
        text.contains("accounts.yml         FAIL  Decryption failed"),
        "{text}"
    );
    assert_eq!(h.screen.mode, Mode::Run);
    ended(
        &mut h,
        [
            Status::Failed("Decryption failed".into()),
            Status::Ignored,
            Status::Done,
        ],
    );
    assert_eq!(h.screen.wake(), Flow::Unchanged, "nothing new");
    assert_eq!(h.screen.mode, Mode::Review);
    assert_eq!(h.screen.menu.selected(), Some(2), "the first restored");
    let text = screen_text(&mut h);
    assert!(text.contains("Check each restored file"), "{text}");
    assert!(text.contains("the right configuration file"), "{text}");
    assert!(text.contains("Files · 1 restored"), "{text}");
    assert!(text.contains("accounts.yml      FAIL"), "{text}");
    assert!(text.contains("rclone.conf       IGNORED"), "{text}");
    assert!(text.contains("settings.yml read-only"), "{text}");
    assert!(text.contains("shell: bash"), "{text}");
    assert!(text.contains("Continue"), "{text}");
    assert!(!text.contains("Ctrl-C stop"), "{text}");
}

/// A failed or ignored file shows why and can't be opened: no content
/// to move into, Tab skips it.
#[test]
fn a_file_not_restored_shows_why() {
    let mut h = harness(Prefill::default());
    submit(&mut h, "someone", "pw");
    ended(
        &mut h,
        [
            Status::Failed("Decryption failed".into()),
            Status::Ignored,
            Status::Failed("x".into()),
        ],
    );
    assert_eq!(
        h.screen.menu.selected(),
        Some(0),
        "none restored: the first"
    );
    let text = screen_text(&mut h);
    assert!(text.contains("No file was restored"), "{text}");
    assert!(
        text.contains("Not restored: Decryption failed. Nothing was"),
        "{text}"
    );
    assert!(!text.contains("read-only"), "{text}");
    assert!(!text.contains("show all"), "{text}");
    assert_eq!(key(&mut h, KeyCode::Enter), Flow::Unchanged);
    key(&mut h, KeyCode::Tab);
    assert_eq!(h.screen.focus_on, Focus::Continue, "no content to focus");
    shift_tab(&mut h);
    key(&mut h, KeyCode::Down);
    assert!(screen_text(&mut h).contains("Not on the restore service for this user"));
}

/// Tab and Shift-Tab go round the menu, the content and Continue; Enter
/// goes into the file and Esc back to the menu; Enter on Continue, `q`
/// anywhere and Esc in the menu close. `c` copies elsewhere, so it no
/// longer continues; Space no longer presses Continue (tui spec, "Keys").
#[test]
fn focus_moves_and_continue_closes() {
    let mut h = harness(Prefill::default());
    place(&h, "settings.yml", "a: 1\n");
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Ignored, Status::Ignored, Status::Done]);
    assert_eq!(h.screen.focus_on, Focus::Menu);
    key(&mut h, KeyCode::Enter);
    assert_eq!(h.screen.focus_on, Focus::File);
    assert_eq!(key(&mut h, KeyCode::Char('q')), Flow::Quit, "q in the file");
    key(&mut h, KeyCode::Esc);
    assert_eq!(h.screen.focus_on, Focus::Menu);
    key(&mut h, KeyCode::Tab);
    assert_eq!(h.screen.focus_on, Focus::File);
    key(&mut h, KeyCode::Tab);
    assert_eq!(h.screen.focus_on, Focus::Continue);
    assert_eq!(key(&mut h, KeyCode::Char('x')), Flow::Unchanged);
    key(&mut h, KeyCode::Tab);
    assert_eq!(h.screen.focus_on, Focus::Menu, "round");
    shift_tab(&mut h);
    assert_eq!(h.screen.focus_on, Focus::Continue);
    key(&mut h, KeyCode::Esc);
    assert_eq!(h.screen.focus_on, Focus::Menu);
    shift_tab(&mut h);
    assert_eq!(key(&mut h, KeyCode::Char(' ')), Flow::Unchanged);
    assert_eq!(key(&mut h, KeyCode::Char('c')), Flow::Unchanged);
    assert_eq!(key(&mut h, KeyCode::Enter), Flow::Quit);
    h.screen.focus_on = Focus::Menu;
    assert_eq!(key(&mut h, KeyCode::Char('q')), Flow::Quit);
    assert_eq!(key(&mut h, KeyCode::Esc), Flow::Quit);
    assert_eq!(key(&mut h, KeyCode::Char('x')), Flow::Unchanged);
}

/// The menu's keys move the selection, clamped, each new file loaded.
#[test]
fn the_menu_moves_and_loads() {
    let mut h = harness(Prefill::default());
    place(&h, "accounts.yml", "first: 1\n");
    place(&h, "settings.yml", "last: 1\n");
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Done]);
    assert!(screen_text(&mut h).contains("first: 1"));
    assert_eq!(key(&mut h, KeyCode::Up), Flow::Unchanged, "at the top");
    key(&mut h, KeyCode::End);
    assert_eq!(h.screen.menu.selected(), Some(2));
    assert!(screen_text(&mut h).contains("last: 1"));
    assert_eq!(key(&mut h, KeyCode::Down), Flow::Unchanged, "at the end");
    key(&mut h, KeyCode::Up);
    assert_eq!(h.screen.menu.selected(), Some(1));
    assert!(h.screen.pane.is_none());
    key(&mut h, KeyCode::Char('k'));
    key(&mut h, KeyCode::Char('j'));
    assert_eq!(h.screen.menu.selected(), Some(1));
    key(&mut h, KeyCode::Home);
    assert_eq!(h.screen.menu.selected(), Some(0));
    assert!(screen_text(&mut h).contains("first: 1"));
    // ← → and `h` `l` choose the previous and next file, from the file
    // too, as the announcement viewer's (tui spec, "Keys", rule 7).
    key(&mut h, KeyCode::Enter);
    assert_eq!(h.screen.focus_on, Focus::File);
    for (code, at) in [
        (KeyCode::Right, 1),
        (KeyCode::Char('l'), 2),
        (KeyCode::Left, 1),
        (KeyCode::Char('h'), 0),
    ] {
        assert_eq!(key(&mut h, code), Flow::Redraw, "{code:?}");
        assert_eq!(h.screen.menu.selected(), Some(at), "{code:?}");
    }
}

/// Every line shows as it is, secrets too (the maintainer, 2026-11-18:
/// values are shown, not hidden); `s` and `a` do nothing.
#[test]
fn every_value_shows_as_it_is() {
    let mut h = harness(Prefill::default());
    place(
        &h,
        "rclone.conf",
        "[remote]\ntype = drive\ntoken = abc123\nclient_secret = shh\nurl = https://u:pw@example.test\n",
    );
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Ignored, Status::Done, Status::Ignored]);
    let text = screen_text(&mut h);
    for line in [
        "type = drive",
        "token = abc123",
        "client_secret = shh",
        "url = https:",
    ] {
        assert!(text.contains(line), "{line}: {text}");
    }
    assert!(!text.contains('•'), "{text}");
    key(&mut h, KeyCode::Enter);
    assert_eq!(key(&mut h, KeyCode::Char('s')), Flow::Unchanged);
    assert_eq!(key(&mut h, KeyCode::Char('a')), Flow::Unchanged);
}

/// The cursor stays in sight: End scrolls to the last line, Home back;
/// the page keys move a page; a long value shown wraps whole.
#[test]
fn the_cursor_scrolls_the_file() {
    let mut h = harness(Prefill::default());
    let token = "t".repeat(150);
    let text = (0..100)
        .map(|n| format!("line{n}: value"))
        .chain([format!("token: {token}")])
        .collect::<Vec<_>>()
        .join("\n");
    place(&h, "accounts.yml", &text);
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Ignored]);
    drawn(&mut h, 100, 30);
    key(&mut h, KeyCode::Enter);
    let cursor = |h: &Harness| h.screen.pane.as_ref().map(|p| p.cursor);
    key(&mut h, KeyCode::End);
    assert_eq!(cursor(&h), Some(100));
    let rows = drawn(&mut h, 100, 30);
    let inner = h.screen.pane.as_ref().expect("pane").inner;
    let joined: String = rows[usize::from(inner.y)..usize::from(inner.bottom())]
        .iter()
        .map(|row| {
            let cells: String = row
                .chars()
                .skip(usize::from(inner.x))
                .take(usize::from(inner.width - 1))
                .collect();
            cells.trim_end().to_owned()
        })
        .collect();
    assert!(joined.contains(&token), "the shown value whole: {joined}");
    assert!(!rows.join("\n").contains("line0: value"), "scrolled");
    key(&mut h, KeyCode::Home);
    assert_eq!(cursor(&h), Some(0));
    assert!(screen_text(&mut h).contains("line0: value"));
    key(&mut h, KeyCode::PageDown);
    assert!(cursor(&h).is_some_and(|c| c > 10), "{:?}", cursor(&h));
    key(&mut h, KeyCode::PageUp);
    assert_eq!(cursor(&h), Some(0));
    key(&mut h, KeyCode::Up);
    assert_eq!(cursor(&h), Some(0));
    key(&mut h, KeyCode::Char('j'));
    key(&mut h, KeyCode::Char('k'));
    assert_eq!(cursor(&h), Some(0));
    assert_eq!(key(&mut h, KeyCode::Char('x')), Flow::Unchanged);
}

/// A file that can't be read says so in the pane.
#[test]
fn an_unreadable_file_says_why() {
    let mut h = harness(Prefill::default());
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Ignored]);
    assert!(screen_text(&mut h).contains("can't read the file"));
}

/// The mouse: a click on a menu row selects it, the wheel over the menu
/// moves it; a click on a line puts the cursor there, the wheel over the
/// file moves it 3 lines; a click on Continue closes. The form and the
/// run ignore the mouse.
#[test]
fn the_mouse_selects_scrolls_and_continues() {
    let mut h = harness(Prefill::default());
    assert_eq!(click(&mut h, 1, 1), Flow::Unchanged, "the form");
    place(&h, "accounts.yml", "a: 1\nb: 2\nc: 3\nd: 4\ne: 5\n");
    place(&h, "settings.yml", "z: 1\n");
    submit(&mut h, "someone", "pw");
    assert_eq!(click(&mut h, 1, 1), Flow::Unchanged, "the run");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Done]);
    drawn(&mut h, 100, 30);
    let rows = h.screen.menu_rows;
    let menu = h.screen.menu_area;
    click(&mut h, rows.x + 1, rows.y + 2);
    assert_eq!(h.screen.menu.selected(), Some(2));
    assert!(screen_text(&mut h).contains("z: 1"));
    assert_eq!(
        click(&mut h, rows.x + 1, rows.y + 5),
        Flow::Unchanged,
        "under the last"
    );
    wheel(&mut h, MouseEventKind::ScrollUp, menu.x + 1, menu.y + 1);
    wheel(&mut h, MouseEventKind::ScrollUp, menu.x + 1, menu.y + 1);
    assert_eq!(h.screen.menu.selected(), Some(0));
    wheel(&mut h, MouseEventKind::ScrollDown, menu.x + 1, menu.y + 1);
    assert_eq!(h.screen.menu.selected(), Some(1));
    assert_eq!(
        click(&mut h, menu.right() + 5, menu.y + 3),
        Flow::Unchanged,
        "no file"
    );
    wheel(&mut h, MouseEventKind::ScrollUp, menu.x + 1, menu.y + 1);
    drawn(&mut h, 100, 30);
    let inner = h.screen.pane.as_ref().expect("pane").inner;
    click(&mut h, inner.x + 1, inner.y + 3);
    assert_eq!(h.screen.focus_on, Focus::File);
    let cursor = |h: &Harness| h.screen.pane.as_ref().map(|p| p.cursor);
    assert_eq!(cursor(&h), Some(3));
    assert_eq!(
        click(&mut h, inner.x + 1, inner.y + 20),
        Flow::Unchanged,
        "under the last line"
    );
    wheel(&mut h, MouseEventKind::ScrollUp, inner.x + 1, inner.y + 1);
    assert_eq!(cursor(&h), Some(0));
    wheel(&mut h, MouseEventKind::ScrollDown, inner.x + 1, inner.y + 1);
    assert_eq!(cursor(&h), Some(3));
    let button = h.screen.button;
    assert_eq!(click(&mut h, button.x + 1, button.y), Flow::Quit);
}

/// The file's escapes and control characters never reach the viewer.
#[test]
fn a_file_is_shown_safely() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("f");
    std::fs::write(&path, "a: \x1b]0;t\x07b\nc\x07d\n").expect("file");
    let lines = read_lines(&path);
    for line in &lines {
        assert!(!line.chars().any(char::is_control), "{line:?}");
    }
    assert_eq!(lines[0], "a: b");
}

/// The answers never show through `Debug`.
#[test]
fn debug_hides_the_answers() {
    let answers = Answers {
        user: Zeroizing::new("someone".into()),
        password: Zeroizing::new(b"pw".to_vec()),
    };
    assert_eq!(format!("{answers:?}"), "Answers([REDACTED])");
    let prefill = Prefill {
        user: Some("someone".into()),
        password: Some(Zeroizing::new(b"pw".to_vec())),
    };
    let shown = format!("{prefill:?}");
    assert!(
        !shown.contains("someone") && !shown.contains("pw"),
        "{shown}"
    );
}

/// With colour off the menu's selected row and the file's cursor are
/// reverse video.
#[test]
fn the_selection_is_reversed_with_colour_off() {
    let mut h = harness(Prefill::default());
    h.screen.theme = Theme::plain();
    place(&h, "accounts.yml", "a: 1\n");
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Ignored]);
    key(&mut h, KeyCode::Enter);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("backend");
    terminal.draw(|f| h.screen.draw(f)).expect("draw");
    let rows = h.screen.menu_rows;
    let inner = h.screen.pane.as_ref().expect("pane").inner;
    let buffer = terminal.backend().buffer();
    assert!(
        buffer[(rows.x + 2, rows.y)]
            .modifier
            .contains(Modifier::REVERSED)
    );
    assert!(
        buffer[(inner.x, inner.y)]
            .modifier
            .contains(Modifier::REVERSED)
    );
    let button = h.screen.button;
    let reversed = |buffer: &ratatui::buffer::Buffer| {
        buffer[(button.x, button.y)]
            .modifier
            .contains(Modifier::REVERSED)
    };
    assert!(!reversed(buffer), "Continue, not focused");
    h.screen.focus_on = Focus::Continue;
    terminal.draw(|f| h.screen.draw(f)).expect("draw");
    assert!(reversed(terminal.backend().buffer()), "Continue, focused");
}

/// Leaving a restored file for one not restored by the wheel over the
/// menu puts the focus back on the menu: the keys still work.
#[test]
fn the_focus_leaves_a_file_that_is_gone() {
    let mut h = harness(Prefill::default());
    place(&h, "accounts.yml", "a: 1\nb: 2\n");
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Ignored]);
    drawn(&mut h, 100, 30);
    let inner = h.screen.pane.as_ref().expect("pane").inner;
    click(&mut h, inner.x + 1, inner.y + 1);
    assert_eq!(h.screen.focus_on, Focus::File);
    let menu = h.screen.menu_area;
    wheel(&mut h, MouseEventKind::ScrollDown, menu.x + 1, menu.y + 1);
    assert!(h.screen.pane.is_none());
    assert_eq!(h.screen.focus_on, Focus::Menu, "no content to focus");
    assert_eq!(key(&mut h, KeyCode::Up), Flow::Redraw);
    // Esc in the content goes back even with none to show.
    h.screen.focus_on = Focus::File;
    h.screen.pane = None;
    assert_eq!(key(&mut h, KeyCode::Esc), Flow::Redraw);
    assert_eq!(h.screen.focus_on, Focus::Menu);
    h.screen.focus_on = Focus::File;
    assert_eq!(key(&mut h, KeyCode::Down), Flow::Unchanged);
}

/// A filled password field's first edit empties both: what is typed
/// then is the password, never the fill's placeholder.
#[test]
fn editing_a_given_password_starts_it_afresh() {
    let mut h = harness(Prefill {
        user: None,
        password: Some(Zeroizing::new(b"from-file".to_vec())),
    });
    typed(&mut h, "someone");
    key(&mut h, KeyCode::Tab);
    // Moving the cursor is no edit: the fill stays.
    key(&mut h, KeyCode::Left);
    assert!(h.screen.given.is_some());
    assert_ne!(h.screen.fields[1].lines().join(""), "");
    key(&mut h, KeyCode::Char('n'));
    assert!(h.screen.given.is_none());
    assert_eq!(h.screen.fields[1].lines(), ["n"]);
    assert_eq!(h.screen.fields[2].lines().join(""), "");
    typed(&mut h, "ew");
    key(&mut h, KeyCode::Tab);
    typed(&mut h, "new");
    key(&mut h, KeyCode::Enter);
    assert_eq!(
        h.answers.try_recv().expect("sent").password.as_slice(),
        b"new"
    );
}

/// The footer names the keys, and no secret key; a reason's own full stop
/// isn't doubled.
#[test]
fn the_footer_and_the_reason_read_right() {
    let mut h = harness(Prefill::default());
    place(&h, "accounts.yml", "a: 1\n");
    submit(&mut h, "someone", "pw");
    ended(
        &mut h,
        [
            Status::Done,
            Status::Failed("it broke.".into()),
            Status::Ignored,
        ],
    );
    let text = screen_text(&mut h);
    assert!(
        text.contains("↑↓ move · Tab menu, file, Continue · ← → file · q quit"),
        "{text}"
    );
    assert!(!text.contains("show"), "{text}");
    key(&mut h, KeyCode::Enter);
    assert!(!screen_text(&mut h).contains("show"));
    key(&mut h, KeyCode::Esc);
    key(&mut h, KeyCode::Down);
    let text = screen_text(&mut h);
    assert!(
        text.contains("Not restored: it broke. Nothing was placed."),
        "{text}"
    );
}

/// The drawn buffer, for its cells' styles.
fn buffer(h: &mut Harness, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("backend");
    terminal.draw(|f| h.screen.draw(f)).expect("draw");
    terminal.backend().buffer().clone()
}

/// The first cell of the first row holding `text`: its position.
fn find(buffer: &ratatui::buffer::Buffer, text: &str) -> Option<(u16, u16)> {
    (0..buffer.area.height).find_map(|y| {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        let at = row.find(text)?;
        // Columns by characters: the border's glyphs are one column each.
        let column = row[..at].chars().count();
        Some((u16::try_from(column).ok()?, y))
    })
}

/// The row holding `text`, as text.
fn row_of(buffer: &ratatui::buffer::Buffer, text: &str) -> String {
    let (_, y) = find(buffer, text).unwrap_or_else(|| panic!("no {text:?}"));
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

/// The form's cells: the title in the accent's colours, the focused
/// field's label in the accent and bold, the others in the text's; the
/// fields two rows apart from the block's second row; the keys separated
/// between, not before the first.
#[test]
fn the_form_marks_its_focus_in_its_cells() {
    let mut h = harness(Prefill::default());
    let b = buffer(&mut h, 100, 30);
    let (x, y) = find(&b, "◆ Restore").expect("the title");
    assert_eq!(
        b[(x, y)].fg,
        h.screen.style(Role::OnAccent).fg.expect("a colour")
    );
    assert_eq!(
        b[(x, y)].bg,
        h.screen.bg(Role::Accent).bg.expect("a colour")
    );
    // `expect`: a style without a colour (a mutant's default) fails here.
    let accent = h.screen.style(Role::Accent).fg.expect("a colour");
    let text = h.screen.style(Role::Text).fg.expect("a colour");
    assert_ne!(accent, text, "the palette tells them apart");
    let (ux, uy) = find(&b, " Username").expect("the user's label");
    let (_, py) = find(&b, " Password").expect("the password's label");
    let (_, ry) = find(&b, " Repeat password").expect("the repeat's label");
    let (_, top) = find(&b, "Saltbox's restore service").expect("the block");
    assert_eq!(
        (uy, py, ry),
        (top + 2, top + 4, top + 6),
        "a field every second row"
    );
    assert_eq!(b[(ux + 1, uy)].fg, accent, "the focused label");
    assert!(b[(ux + 1, uy)].modifier.contains(Modifier::BOLD));
    assert_eq!(b[(ux + 1, py)].fg, text, "a label not focused");
    assert!(!b[(ux + 1, py)].modifier.contains(Modifier::BOLD));
    let footer = row_of(&b, "next field");
    assert!(
        footer
            .trim_start()
            .starts_with("Tab next field · Enter next"),
        "{footer}"
    );
    assert_eq!(footer.matches(" · ").count(), 2, "{footer}");
    // The focus moves with Tab: the password's label takes the accent.
    key(&mut h, KeyCode::Tab);
    let b = buffer(&mut h, 100, 30);
    assert_eq!(b[(ux + 1, py)].fg, accent);
    assert_eq!(b[(ux + 1, uy)].fg, text);
}

/// The viewer's borders: the focused block's in the accent, the other's
/// the border's, as the focus moves from the menu to the file.
#[test]
fn the_focused_block_has_the_accents_border() {
    let mut h = harness(Prefill::default());
    place(&h, "accounts.yml", "a: 1\n");
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Ignored]);
    let accent = h.screen.style(Role::Accent).fg.expect("a colour");
    let border = h.screen.style(Role::Border).fg.expect("a colour");
    assert_ne!(accent, border, "the palette tells them apart");
    let b = buffer(&mut h, 100, 30);
    let menu = h.screen.menu_area;
    let pane = h.screen.pane.as_ref().expect("pane").inner;
    // Each block's left border, a row into it: the first `│` left of
    // the pane's inside.
    let menu_edge = (menu.x, menu.y + 1);
    let pane_edge = (
        (0..pane.x)
            .rev()
            .find(|x| b[(*x, pane.y)].symbol() == "│")
            .expect("the pane's border"),
        pane.y,
    );
    assert_eq!(b[menu_edge].symbol(), "│");
    assert_eq!(b[menu_edge].fg, accent, "the menu, focused");
    assert_eq!(b[pane_edge].fg, border, "the file, not focused");
    key(&mut h, KeyCode::Enter);
    assert_eq!(h.screen.focus_on, Focus::File);
    let b = buffer(&mut h, 100, 30);
    assert_eq!(b[menu_edge].fg, border, "the menu, not focused");
    assert_eq!(b[pane_edge].fg, accent, "the file, focused");
}

/// Lines that fill the pane exactly take its whole width: no column goes
/// to a scrollbar, so a line as wide as the pane stays one row and the
/// last line shows.
#[test]
fn a_file_that_fits_takes_the_whole_width() {
    let mut h = harness(Prefill::default());
    place(&h, "accounts.yml", "a: 1\n");
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Ignored]);
    drawn(&mut h, 100, 30);
    let inner = h.screen.pane.as_ref().expect("pane").inner;
    // `inner.height` lines in all, the first as wide as the pane.
    let wide = "w".repeat(usize::from(inner.width));
    let mut lines = vec![wide.clone()];
    lines.extend((2..usize::from(inner.height)).map(|i| format!("line{i}: x")));
    lines.push("last: here".into());
    place(&h, "accounts.yml", &(lines.join("\n") + "\n"));
    h.screen.load();
    let text = screen_text(&mut h);
    assert!(text.contains("last: here"), "{text}");
    assert!(text.contains(&wide), "{text}");
}

/// Loading a file with nothing to show takes the focus off the file
/// only: on Continue it stays there.
#[test]
fn loading_keeps_a_focus_off_the_file() {
    let mut h = harness(Prefill::default());
    place(&h, "accounts.yml", "a: 1\n");
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Ignored]);
    h.screen.focus_on = Focus::Continue;
    h.screen.menu.select(Some(1));
    h.screen.load();
    assert!(h.screen.pane.is_none());
    assert_eq!(h.screen.focus_on, Focus::Continue);
}

/// `Debug` writes the prefill's shape with each answer redacted.
#[test]
fn debug_writes_the_prefills_shape() {
    let prefill = Prefill {
        user: Some("someone".into()),
        password: Some(Zeroizing::new(b"pw".to_vec())),
    };
    assert_eq!(
        format!("{prefill:?}"),
        r#"Prefill { user: Some("[REDACTED]"), password: Some("[REDACTED]") }"#
    );
    let given = Prefill {
        user: None,
        password: None,
    };
    assert_eq!(
        format!("{given:?}"),
        "Prefill { user: None, password: None }"
    );
}

/// A cursor line taller than the pane shows from its top, and stays so
/// from one draw to the next (the view didn't jump between its bottom
/// and its top).
#[test]
fn a_tall_cursor_line_keeps_its_view() {
    let mut h = harness(Prefill::default());
    place(&h, "accounts.yml", "a: 1\n");
    submit(&mut h, "someone", "pw");
    ended(&mut h, [Status::Done, Status::Ignored, Status::Ignored]);
    drawn(&mut h, 100, 30);
    let inner = h.screen.pane.as_ref().expect("pane").inner;
    let tall = "t".repeat(usize::from(inner.width) * usize::from(inner.height + 2));
    place(&h, "accounts.yml", &format!("{tall}\nnext: x\n"));
    h.screen.load();
    let offsets: Vec<u16> = (0..4)
        .map(|_| {
            drawn(&mut h, 100, 30);
            h.screen.pane.as_ref().expect("pane").scroll.offset().y
        })
        .collect();
    assert_eq!(offsets, [0, 0, 0, 0]);
}
