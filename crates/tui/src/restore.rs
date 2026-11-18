//! `sb restore`'s screen (ops spec, "`sb restore` (hidden)", decision 32;
//! the maintainer, 2026-11-16 and 2026-11-17): a form for the user name
//! and the password, the service's files as a live list, each row's
//! outcome as it comes, then a viewer of the restored files to check,
//! closed by Continue.
//!
//! - **The loop** is the module's `Tui` and `App` (ratatui's async
//!   recipe). The fetch is the caller's future, started once the form is
//!   submitted and driven beside the screen in one `tokio::select!` on
//!   the run's task (<https://docs.rs/tokio/1.54.1/tokio/macro.select.html>);
//!   it reports each file through [`Updates`], which wakes the screen
//!   (`Tui::waker`). When the screen closes, the fetch is dropped: a
//!   request under way is cancelled.
//! - **The fields** are ratatui-textarea lines, the passwords masked with
//!   its `set_mask_char`
//!   (<https://docs.rs/ratatui-textarea/0.9.3/ratatui_textarea/struct.TextArea.html#method.set_mask_char>).
//! - **The viewer** is the announcement viewer's menu beside a pane
//!   ([`crate::split`]: the layout, the clamped list movement, a click's
//!   row): the menu a `List` with a `ListState`, the pane a tui-scrollview
//!   `ScrollView`
//!   (<https://docs.rs/tui-scrollview/0.6.8/tui_scrollview/struct.ScrollView.html>)
//!   of the file's wrapped lines with a line cursor, each line shown as it
//!   is, secrets too (the maintainer, 2026-11-18: values are shown, not
//!   hidden; tui spec, "Keys: one map for every screen").
//! - **Secrets typed:** the answers leave the form in buffers cleared on
//!   drop; the text areas are emptied once read (their own `String`s are
//!   not cleared: a known limit). Nothing here logs a value.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;

use crossterm::event::{
    Event as Input, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect, Size};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Padding, Paragraph, Wrap};
use ratatui_textarea::TextArea;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::theme::{Role, Theme};
use sb_term_ui::DisplayText;
use tokio::sync::{Notify, mpsc, oneshot};
use tui_scrollview::ScrollViewState;
use zeroize::Zeroizing;

use crate::hints::{Group, Hint, Key, Keys, Meaning, Overlay};
use crate::split;
use crate::term::{App, Ending, Flow, Tui};

/// The passwords' mask in the form.
pub const MASK_CHAR: char = '•';

/// The form's keys, for the overlay (F1: the fields take `?`).
pub(crate) const FORM_KEYS: Keys = Keys {
    screen: "restore form",
    groups: &[Group {
        title: "Fields",
        keys: &[
            Key {
                keys: &["Tab", "Shift-Tab"],
                what: "the next, previous field",
                meaning: Meaning::Part,
            },
            Key {
                keys: &["↑", "↓"],
                what: "the previous, next field",
                meaning: Meaning::Move,
            },
            Key {
                keys: &["Enter"],
                what: "the next field; restores from the last",
                meaning: Meaning::Open,
            },
            Key {
                keys: &["Esc"],
                what: "cancel the restore",
                meaning: Meaning::Back,
            },
            Key {
                keys: &["Ctrl+C"],
                what: "stop (exit 130)",
                meaning: Meaning::Stop,
            },
            Key {
                keys: &["F1"],
                what: "these keys",
                meaning: Meaning::Help,
            },
        ],
    }],
    mouse: "none in the form",
};

/// The live list's keys.
pub(crate) const RUN_KEYS: Keys = Keys {
    screen: "restore run",
    groups: &[Group {
        title: "Leave",
        keys: &[
            Key {
                keys: &["Ctrl+C"],
                what: "stop the restore (exit 130)",
                meaning: Meaning::Stop,
            },
            Key {
                keys: &["?", "F1"],
                what: "these keys",
                meaning: Meaning::Help,
            },
        ],
    }],
    mouse: "none while the files come",
};

/// The viewer's keys.
pub(crate) const REVIEW_KEYS: Keys = Keys {
    screen: "restore review",
    groups: &[
        Group {
            title: "Move",
            keys: &[
                Key {
                    keys: &["↑", "↓", "k", "j"],
                    what: "a file, or a line",
                    meaning: Meaning::Move,
                },
                Key {
                    keys: &["PgUp", "PgDn", "b", "Space"],
                    what: "a page of the file",
                    meaning: Meaning::Page,
                },
                Key {
                    keys: &["Home", "End", "g", "G"],
                    what: "the first, the last",
                    meaning: Meaning::Ends,
                },
                Key {
                    keys: &["←", "→", "h", "l"],
                    what: "the previous, next file",
                    meaning: Meaning::Direction,
                },
            ],
        },
        Group {
            title: "Parts",
            keys: &[
                Key {
                    keys: &["Tab", "Shift-Tab"],
                    what: "the menu, the file, Continue",
                    meaning: Meaning::Part,
                },
                Key {
                    keys: &["Enter"],
                    what: "into the file, or Continue",
                    meaning: Meaning::Open,
                },
                Key {
                    keys: &["Esc"],
                    what: "back to the menu, or close",
                    meaning: Meaning::Back,
                },
            ],
        },
        Group {
            title: "Leave",
            keys: &[
                Key {
                    keys: &["q"],
                    what: "close",
                    meaning: Meaning::Leave,
                },
                Key {
                    keys: &["Ctrl+C"],
                    what: "stop (exit 130)",
                    meaning: Meaning::Stop,
                },
                Key {
                    keys: &["?", "F1"],
                    what: "these keys",
                    meaning: Meaning::Help,
                },
            ],
        },
    ],
    mouse: "a click selects a file or a line, or presses Continue; the wheel moves",
};

/// Lines the wheel moves the pane's cursor by.
const WHEEL_LINES: usize = 3;

/// The form's fields, in order.
const FIELDS: [&str; 3] = ["Username", "Password", "Repeat password"];

/// sb-go's mismatch text.
const MISMATCH: &str = "Error: passwords do not match or are empty";

/// What the flags gave, filling the form's fields.
#[derive(Default)]
pub struct Prefill {
    pub user: Option<String>,
    pub password: Option<Zeroizing<Vec<u8>>>,
}

/// The answers, once the form is submitted (or both flags gave them).
pub struct Answers {
    pub user: Zeroizing<String>,
    pub password: Zeroizing<Vec<u8>>,
}

/// Neither answer shows through `Debug` (ops decision 10).
impl std::fmt::Debug for Answers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Answers([REDACTED])")
    }
}

/// Neither answer shows through `Debug`.
impl std::fmt::Debug for Prefill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prefill")
            .field("user", &self.user.as_ref().map(|_| "[REDACTED]"))
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// A file's state on the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Fetching,
    /// Fetched, decrypted and placed.
    Done,
    /// Not on the service (a status other than 200).
    Ignored,
    /// Fetched but not placed, with why.
    Failed(String),
}

/// One file's row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub status: Status,
}

/// What the fetch reports.
#[derive(Debug)]
pub enum Update {
    /// The file at this index is being fetched.
    Fetching(usize),
    /// The file at this index ended so.
    Finished(usize, Status),
    /// The run is over, with the summary line to show.
    Summary(String),
}

/// The fetch's way to the screen: each update queued, then the screen
/// woken (`Notify` keeps one permit, so none is lost).
#[derive(Clone, Debug)]
pub struct Updates {
    tx: mpsc::UnboundedSender<Update>,
    wake: Arc<Notify>,
}

impl Updates {
    /// Send `update` to the screen.
    pub fn send(&self, update: Update) {
        let _ = self.tx.send(update);
        self.wake.notify_one();
    }
}

/// How the screen closed, and the list as it was.
#[derive(Debug)]
pub struct Closed {
    pub ending: Ending,
    /// Esc in the form: nothing was started.
    pub cancelled: bool,
    /// The run started (the form was submitted, or skipped).
    pub started: bool,
    pub rows: Vec<Row>,
    /// The summary line, if the run finished.
    pub summary: Option<String>,
}

/// What the screen shows besides the form and the list.
#[derive(Debug)]
pub struct Setup<'a> {
    /// The files, in order.
    pub files: &'a [&'a str],
    /// Where they are placed, for the viewer.
    pub root: PathBuf,
    /// The service's base, as the list's heading shows it.
    pub base: String,
    pub theme: Theme,
}

/// The restore screen on stdout's terminal until it closes: the form
/// (skipped when `prefill` has both answers), then `fetch` with the
/// answers and the screen's [`Updates`], driven beside the screen; once
/// the run ends, the viewer until Continue, Ctrl-C, or the run's token.
/// The fetch is dropped (cancelled) when the screen closes.
///
/// # Errors
///
/// The I/O error when the screen can't be set up, drawn or read, or
/// can't be restored (reported whatever the ending, cli-L5).
pub async fn screen<F, Fut>(
    shutdown: &Shutdown,
    setup: Setup<'_>,
    prefill: Prefill,
    fetch: F,
) -> io::Result<Closed>
where
    F: FnOnce(Answers, Updates) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut tui = Tui::enter(shutdown)?;
    let (answers_tx, answers_rx) = oneshot::channel();
    let (tx, rx) = mpsc::unbounded_channel();
    let updates = Updates {
        tx,
        wake: tui.waker(),
    };
    let mut app = Screen::new(setup, prefill, answers_tx, rx);
    let run = async move {
        if let Ok(answers) = answers_rx.await {
            fetch(answers, updates).await;
        }
    };
    let mut run = pin!(run);
    let mut ran = false;
    let ending = loop {
        tokio::select! {
            ending = tui.run(&mut app) => break ending,
            () = &mut run, if !ran => ran = true,
        }
    };
    let left = tui.leave().await;
    tracing::info!(
        ending = ?ending.as_ref().ok(),
        started = app.started,
        "restore screen closed"
    );
    let ending = crate::ended(ending, left)?;
    Ok(app.closed(ending))
}

/// Which part of the screen shows.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Form,
    /// The live list, while the fetch runs.
    Run,
    /// The viewer, once the run ends.
    Review,
}

/// The viewer's part that takes the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Menu,
    File,
    Continue,
}

/// The restore screen (an [`App`]).
pub(crate) struct Screen {
    mode: Mode,
    theme: Theme,
    root: PathBuf,
    base: String,
    fields: [TextArea<'static>; 3],
    focus: usize,
    /// The password file's bytes, while neither password field is edited.
    given: Option<Zeroizing<Vec<u8>>>,
    error: Option<&'static str>,
    answers: Option<oneshot::Sender<Answers>>,
    updates: mpsc::UnboundedReceiver<Update>,
    rows: Vec<Row>,
    summary: Option<String>,
    cancelled: bool,
    started: bool,
    focus_on: Focus,
    menu: ListState,
    /// The selected file's content, when it was restored.
    pane: Option<Pane>,
    /// From the last draw, for the mouse: the menu, its rows inside the
    /// border, and the Continue button.
    menu_area: Rect,
    menu_rows: Rect,
    button: Rect,
    overlay: Overlay,
}

impl Screen {
    pub(crate) fn new(
        setup: Setup<'_>,
        prefill: Prefill,
        answers: oneshot::Sender<Answers>,
        updates: mpsc::UnboundedReceiver<Update>,
    ) -> Self {
        let mut screen = Screen {
            mode: Mode::Form,
            theme: setup.theme,
            root: setup.root,
            base: setup.base,
            fields: [field(false), field(true), field(true)],
            focus: usize::from(prefill.user.is_some()),
            given: None,
            error: None,
            answers: Some(answers),
            updates,
            rows: setup
                .files
                .iter()
                .map(|name| Row {
                    name: (*name).to_owned(),
                    status: Status::Waiting,
                })
                .collect(),
            summary: None,
            cancelled: false,
            started: false,
            focus_on: Focus::Menu,
            menu: ListState::default(),
            pane: None,
            menu_area: Rect::default(),
            menu_rows: Rect::default(),
            button: Rect::default(),
            overlay: Overlay::default(),
        };
        match (prefill.user, prefill.password) {
            (Some(user), Some(password)) if !user.is_empty() && !password.is_empty() => {
                screen.start(Answers {
                    user: Zeroizing::new(user),
                    password,
                });
            }
            (user, password) => {
                if let Some(user) = user {
                    screen.fields[0].insert_str(user);
                }
                if let Some(password) = password {
                    // Both password fields show the file's (its length in
                    // the mask); unless one is edited, its bytes are sent
                    // as read, not the fields' text, which can't hold every
                    // byte (no newline, no invalid UTF-8).
                    let shown = Zeroizing::new("x".repeat(password.len()));
                    screen.fields[1].insert_str(shown.as_str());
                    screen.fields[2].insert_str(shown.as_str());
                    screen.given = Some(password);
                }
            }
        }
        screen
    }

    /// How the screen ended, with the rows as the fetch left them: the
    /// updates still queued are taken first (a file placed in the same
    /// tick as Ctrl-C is listed).
    fn closed(mut self, ending: Ending) -> Closed {
        self.wake();
        Closed {
            ending,
            cancelled: self.cancelled,
            started: self.started,
            rows: self.rows,
            summary: self.summary,
        }
    }

    fn style(&self, role: Role) -> Style {
        Style::from(self.theme.style(role))
    }

    fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.theme.color(role)))
    }

    /// The form's answers when they hold, else the form's error.
    fn submit(&mut self) {
        let user = Zeroizing::new(self.fields[0].lines().join(""));
        let password = Zeroizing::new(self.fields[1].lines().join(""));
        let repeat = Zeroizing::new(self.fields[2].lines().join(""));
        self.error = if user.is_empty() {
            Some("Enter the user name")
        } else if password.is_empty() || *password != *repeat {
            Some(MISMATCH)
        } else {
            None
        };
        if self.error.is_some() {
            return;
        }
        let password = self
            .given
            .take()
            .unwrap_or_else(|| Zeroizing::new(password.as_bytes().to_vec()));
        // The fields emptied: only the answers keep the values.
        self.fields = [field(false), field(true), field(true)];
        self.start(Answers { user, password });
    }

    /// The run started with `answers`: the list shows.
    fn start(&mut self, answers: Answers) {
        if let Some(sender) = self.answers.take() {
            let _ = sender.send(answers);
        }
        self.started = true;
        self.mode = Mode::Run;
        tracing::info!("restore started");
    }

    /// A key in the form. Enter, `\n`, `\r` and Ctrl-M, which a field
    /// would take as a new line, move on instead, as ratatui-textarea's
    /// single-line example does
    /// (<https://github.com/ratatui/ratatui-textarea/blob/v0.9.3/examples/single_line.rs>);
    /// every other key is the field's (`TextArea::input`: editing and
    /// the cursor).
    fn form_key(&mut self, key: KeyEvent) -> Flow {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let code = match key.code {
            KeyCode::Char('\n' | '\r') => KeyCode::Enter,
            KeyCode::Char('m') if ctrl => KeyCode::Enter,
            code => code,
        };
        match code {
            KeyCode::Esc => {
                self.cancelled = true;
                return Flow::Quit;
            }
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % FIELDS.len(),
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = (self.focus + FIELDS.len() - 1) % FIELDS.len();
            }
            KeyCode::Enter if self.focus + 1 < FIELDS.len() => self.focus += 1,
            KeyCode::Enter => self.submit(),
            _ => {
                if self.focus > 0 && self.given.is_some() {
                    // The first edit of a filled password field empties
                    // both: the fields' text is the password from then on,
                    // never the fill's placeholder.
                    let mut probe = self.fields[self.focus].clone();
                    if probe.input(key) {
                        self.given = None;
                        self.fields[1] = field(true);
                        self.fields[2] = field(true);
                    }
                }
                self.fields[self.focus].input(key);
            }
        }
        Flow::Redraw
    }

    /// The run ended: the viewer, on the first restored file (else the
    /// first), the menu focused.
    fn review(&mut self) {
        self.mode = Mode::Review;
        self.focus_on = Focus::Menu;
        let first = self
            .rows
            .iter()
            .position(|row| row.status == Status::Done)
            .unwrap_or(0);
        self.menu.select(Some(first));
        self.load();
    }

    /// The selected file's pane: its lines when it was restored, else none.
    fn load(&mut self) {
        let row = self.menu.selected().and_then(|i| self.rows.get(i));
        self.pane = match row {
            Some(row) if row.status == Status::Done => {
                tracing::info!(file = row.name, "restored file viewed");
                Some(Pane::new(read_lines(&self.root.join(&row.name))))
            }
            _ => None,
        };
        if self.pane.is_none() && self.focus_on == Focus::File {
            self.focus_on = Focus::Menu;
        }
    }

    /// The menu's selection moved by `movement` ([`split::move_list`]);
    /// a new file is loaded.
    fn move_menu(&mut self, movement: impl FnOnce(&mut ListState)) -> Flow {
        if split::move_list(&mut self.menu, self.rows.len(), movement) {
            self.load();
            Flow::Redraw
        } else {
            Flow::Unchanged
        }
    }

    /// The parts Tab moves between, in order: the content only when the
    /// selected file has one.
    fn focus_order(&self) -> Vec<Focus> {
        let mut order = vec![Focus::Menu];
        if self.pane.is_some() {
            order.push(Focus::File);
        }
        order.push(Focus::Continue);
        order
    }

    /// Focus moved by `by` (1 for Tab, -1 for Shift-Tab), round.
    fn cycle(&mut self, by: isize) -> Flow {
        let order = self.focus_order();
        let at = order.iter().position(|f| *f == self.focus_on).unwrap_or(0);
        let len = order.len().cast_signed();
        let next = (at.cast_signed() + by).rem_euclid(len);
        self.focus_on = order[next.cast_unsigned()];
        Flow::Redraw
    }

    fn review_key(&mut self, key: KeyEvent) -> Flow {
        // `q` closes from any part; ← → and `h` `l` choose the previous
        // and next file from any part, as the announcement viewer's (tui
        // spec, "Keys: one map for every screen", rules 6 and 7).
        match key.code {
            KeyCode::Char('q') => return Flow::Quit,
            KeyCode::Left | KeyCode::Char('h') => {
                return self.move_menu(ListState::select_previous);
            }
            KeyCode::Right | KeyCode::Char('l') => return self.move_menu(ListState::select_next),
            KeyCode::Tab => return self.cycle(1),
            KeyCode::BackTab => return self.cycle(-1),
            _ => {}
        }
        match self.focus_on {
            Focus::Menu => self.menu_key(key.code),
            Focus::File => self.file_key(key.code),
            Focus::Continue => match key.code {
                KeyCode::Enter => Flow::Quit,
                KeyCode::Esc => {
                    self.focus_on = Focus::Menu;
                    Flow::Redraw
                }
                _ => Flow::Unchanged,
            },
        }
    }

    fn menu_key(&mut self, code: KeyCode) -> Flow {
        match code {
            KeyCode::Esc => Flow::Quit,
            KeyCode::Down | KeyCode::Char('j') => self.move_menu(ListState::select_next),
            KeyCode::Up | KeyCode::Char('k') => self.move_menu(ListState::select_previous),
            KeyCode::Home | KeyCode::Char('g') => self.move_menu(ListState::select_first),
            KeyCode::End | KeyCode::Char('G') => self.move_menu(ListState::select_last),
            KeyCode::Enter if self.pane.is_some() => {
                self.focus_on = Focus::File;
                Flow::Redraw
            }
            _ => Flow::Unchanged,
        }
    }

    fn file_key(&mut self, code: KeyCode) -> Flow {
        if code == KeyCode::Esc {
            self.focus_on = Focus::Menu;
            return Flow::Redraw;
        }
        let Some(pane) = self.pane.as_mut() else {
            return Flow::Unchanged;
        };
        let screenful = usize::from(pane.inner.height.max(1));
        let at = pane.cursor;
        let to = match code {
            KeyCode::Down | KeyCode::Char('j') => at.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => at.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => at.saturating_add(screenful),
            KeyCode::PageUp | KeyCode::Char('b') => at.saturating_sub(screenful),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => usize::MAX,
            _ => return Flow::Unchanged,
        };
        pane.cursor_to(to);
        Flow::Redraw
    }

    fn mouse(&mut self, mouse: MouseEvent) -> Flow {
        if self.mode != Mode::Review {
            return Flow::Unchanged;
        }
        let at = Position::new(mouse.column, mouse.row);
        let over_menu = self.menu_area.contains(at);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if self.button.contains(at) => Flow::Quit,
            MouseEventKind::Down(MouseButton::Left) if over_menu => {
                let Some(row) = split::clicked(&self.menu, self.menu_rows, at, self.rows.len())
                else {
                    return Flow::Unchanged;
                };
                self.focus_on = Focus::Menu;
                self.move_menu(|menu| menu.select(Some(row)));
                Flow::Redraw
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(pane) = self.pane.as_mut() else {
                    return Flow::Unchanged;
                };
                let Some(line) = pane.line_at(at) else {
                    return Flow::Unchanged;
                };
                pane.cursor_to(line);
                self.focus_on = Focus::File;
                Flow::Redraw
            }
            MouseEventKind::ScrollDown if over_menu => self.move_menu(ListState::select_next),
            MouseEventKind::ScrollUp if over_menu => self.move_menu(ListState::select_previous),
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let Some(pane) = self.pane.as_mut() else {
                    return Flow::Unchanged;
                };
                let to = if mouse.kind == MouseEventKind::ScrollDown {
                    pane.cursor.saturating_add(WHEEL_LINES)
                } else {
                    pane.cursor.saturating_sub(WHEEL_LINES)
                };
                pane.cursor_to(to);
                Flow::Redraw
            }
            _ => Flow::Unchanged,
        }
    }

    fn title(&self, context: &str) -> Line<'static> {
        Line::from(vec![
            Span::styled(
                " ◆ Restore ",
                self.style(Role::OnAccent).patch(self.bg(Role::Accent)),
            ),
            Span::raw("  "),
            Span::styled(context.to_owned(), self.style(Role::Muted)),
        ])
    }

    /// The footer: `hints` and `tail`, fitted to `width` (tui spec,
    /// "Keys", Hints).
    fn hints(&self, hints: &[Hint], tail: &[Hint], width: u16) -> Line<'static> {
        crate::hints::footer(&self.theme, None, hints, tail, width)
    }

    fn draw_form(&mut self, frame: &mut Frame<'_>, body: Rect, footer: Rect) {
        // ratatui's `Rect::centered`
        // (<https://docs.rs/ratatui-core/0.1.2/ratatui_core/layout/struct.Rect.html#method.centered>).
        let area = body.centered(Constraint::Max(60), Constraint::Length(10));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(self.style(Role::Border))
            .title(Span::styled(
                " Saltbox's restore service ",
                self.style(Role::Accent).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner);
        for (i, label) in FIELDS.iter().enumerate() {
            let [label_area, field_area] =
                Layout::horizontal([Constraint::Length(18), Constraint::Fill(1)])
                    .areas(rows[1 + i * 2]);
            let focused = i == self.focus;
            let label_style = if focused {
                self.style(Role::Accent).add_modifier(Modifier::BOLD)
            } else {
                self.style(Role::Text)
            };
            frame.render_widget(
                Paragraph::new(Span::styled(format!(" {label}"), label_style)),
                label_area,
            );
            // The focused field shows its cursor; the others none.
            let cursor = if focused {
                Style::new().add_modifier(Modifier::REVERSED)
            } else {
                Style::new()
            };
            self.fields[i].set_cursor_style(cursor);
            self.fields[i].set_style(self.style(Role::Text));
            frame.render_widget(&self.fields[i], field_area);
        }
        if let Some(error) = self.error {
            frame.render_widget(
                Paragraph::new(Span::styled(format!(" {error}"), self.style(Role::Problem))),
                rows[7],
            );
        }
        frame.render_widget(
            Paragraph::new(self.hints(
                &[
                    ("Tab", "next field"),
                    ("Enter", "next (restores on the last)"),
                ],
                &[(FORM_KEYS.help_key(), "keys"), ("Esc", "cancel")],
                footer.width,
            )),
            footer,
        );
    }

    fn status_span(&self, status: &Status) -> Span<'static> {
        match status {
            Status::Waiting => Span::styled("waiting", self.style(Role::Faint)),
            Status::Fetching => Span::styled("fetching…", self.style(Role::Accent)),
            Status::Done => Span::styled("DONE", self.style(Role::Ok).add_modifier(Modifier::BOLD)),
            Status::Ignored => Span::styled("IGNORED", self.style(Role::Muted)),
            Status::Failed(_) => Span::styled(
                "FAIL",
                self.style(Role::Problem).add_modifier(Modifier::BOLD),
            ),
        }
    }

    fn draw_run(&mut self, frame: &mut Frame<'_>, body: Rect, footer: Rect) {
        let [heading, _, list, _, summary] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(u16::try_from(self.rows.len()).unwrap_or(u16::MAX)),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(body);
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    " Fetching files from {}",
                    DisplayText::from_bytes(self.base.as_bytes(), false).as_str()
                ),
                self.style(Role::Muted),
            )),
            heading,
        );
        let items: Vec<ListItem<'static>> = self
            .rows
            .iter()
            .map(|row| {
                let mut spans = vec![
                    Span::styled(format!(" {:<20} ", row.name), self.style(Role::Text)),
                    self.status_span(&row.status),
                ];
                if let Status::Failed(reason) = &row.status {
                    spans.push(Span::styled(format!("  {reason}"), self.style(Role::Muted)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        frame.render_widget(List::new(items), list);
        frame.render_widget(
            Paragraph::new(Span::styled(" Restoring…", self.style(Role::Muted))),
            summary,
        );
        frame.render_widget(
            Paragraph::new(self.hints(
                &[],
                &[(RUN_KEYS.help_key(), "keys"), ("Ctrl+C", "stop")],
                footer.width,
            )),
            footer,
        );
    }

    /// A menu row: the name, then its mark, a space either side.
    fn menu_row(&self, row: &Row) -> ListItem<'static> {
        ListItem::new(Line::from(vec![
            Span::styled(format!(" {:<17} ", row.name), self.style(Role::Text)),
            self.status_span(&row.status),
            Span::raw(" "),
        ]))
    }

    /// A block, its border the accent's when `focused`.
    fn block(&self, focused: bool) -> Block<'static> {
        let border = if focused {
            self.style(Role::Accent)
        } else {
            self.style(Role::Border)
        };
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
    }

    fn draw_review(&mut self, frame: &mut Frame<'_>, body: Rect, footer: Rect) {
        let [check, _, body, _, bottom] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(body);
        let restored = self
            .rows
            .iter()
            .filter(|row| row.status == Status::Done)
            .count();
        let check_line = if restored == 0 {
            Line::from(Span::styled(
                "No file was restored: each one's reason shows when it is selected.",
                self.style(Role::Problem),
            ))
        } else {
            Line::from(vec![
                Span::styled(
                    "Check each restored file: ",
                    self.style(Role::Text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "it should be decrypted, correct, and the right configuration file. Then Continue.",
                    self.style(Role::Text),
                ),
            ])
        };
        frame.render_widget(
            Paragraph::new(check_line)
                .wrap(Wrap { trim: false })
                .block(Block::new().padding(Padding::horizontal(1))),
            check,
        );
        let rows: Vec<ListItem<'static>> = self.rows.iter().map(|row| self.menu_row(row)).collect();
        let title = Line::from(vec![
            Span::styled(
                " Files ",
                self.style(Role::Accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("· {restored} restored "), self.style(Role::Muted)),
        ]);
        let [menu_area, pane_area] = split::areas(body, &rows, &title);
        let block = self.block(self.focus_on == Focus::Menu).title(title);
        self.menu_area = menu_area;
        self.menu_rows = block.inner(menu_area);
        frame.render_stateful_widget(
            List::new(rows)
                .block(block)
                .highlight_style(split::selected(&self.theme)),
            menu_area,
            &mut self.menu,
        );
        self.draw_pane(frame, pane_area);
        self.draw_bottom(frame, bottom, footer);
    }

    /// The selected file's content, or why it has none.
    fn draw_pane(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let Some(row) = self.menu.selected().and_then(|i| self.rows.get(i)) else {
            return;
        };
        let mut title = vec![Span::styled(
            format!(" {} ", row.name),
            self.style(Role::Accent).add_modifier(Modifier::BOLD),
        )];
        let why = match &row.status {
            Status::Done => None,
            Status::Failed(reason) => Some(format!(
                "Not restored: {}. Nothing was placed.",
                reason.trim_end_matches('.')
            )),
            _ => Some(
                "Not on the restore service for this user: no backup uploaded it, so nothing was placed."
                    .to_owned(),
            ),
        };
        if why.is_none() {
            title.push(Span::styled("read-only ", self.style(Role::Muted)));
        }
        let block = self
            .block(self.focus_on == Focus::File)
            .title(Line::from(title))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if let Some(why) = why {
            frame.render_widget(
                Paragraph::new(why)
                    .style(self.style(Role::Muted))
                    .wrap(Wrap { trim: false }),
                inner,
            );
            return;
        }
        let styles = PaneStyles {
            text: self.style(Role::Text),
            cursor: (self.focus_on == Focus::File).then(|| split::selected(&self.theme)),
        };
        if let Some(pane) = self.pane.as_mut() {
            pane.draw(frame, inner, &styles);
        }
    }

    /// The Continue button, then the keys.
    fn draw_bottom(&mut self, frame: &mut Frame<'_>, bottom: Rect, footer: Rect) {
        // Focused, filled in the accent (reverse video with colour off,
        // as the selection's rule); else the accent's bold text.
        let label = "[ Continue ]";
        let style = if self.focus_on == Focus::Continue {
            split::reversed_without_colour(
                &self.theme,
                self.style(Role::OnAccent)
                    .patch(self.bg(Role::Accent))
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            self.style(Role::Accent).add_modifier(Modifier::BOLD)
        };
        let width = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
        let [_, button, _] = Layout::horizontal([
            Constraint::Length(1),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .areas(bottom);
        self.button = button;
        frame.render_widget(Paragraph::new(Span::styled(label, style)), button);
        let hints = [
            ("Enter", "open"),
            ("Tab", "part"),
            ("← →", "file"),
            ("↑↓", "move"),
        ];
        frame.render_widget(
            Paragraph::new(self.hints(
                &hints,
                &[(REVIEW_KEYS.help_key(), "keys"), ("q", "quit")],
                footer.width,
            )),
            footer,
        );
    }
}

/// The styles a pane draws with.
struct PaneStyles {
    text: Style,
    /// The cursor's bar, when the content is focused.
    cursor: Option<Style>,
}

/// A restored file in the viewer's pane: its lines, a line cursor, the
/// scroll, and each line's rows from the last draw (for the cursor's
/// sight and a click).
///
/// Why not ratatui's `List`, whose `ListState` keeps a selection in
/// sight: its items don't wrap (ratatui-widgets 0.3.2's `reflow` is
/// private), and a long value, an rclone token, must show whole. Why not
/// ratatui-textarea's cursor: the pane is read-only. tui-scrollview has no "scroll this into view", so
/// the offset is set from each line's rows (`ScrollViewState::set_offset`).
struct Pane {
    /// Each line, made safe.
    lines: Vec<String>,
    cursor: usize,
    scroll: ScrollViewState,
    /// Each line's first row and its row count, at the last draw.
    starts: Vec<u16>,
    heights: Vec<u16>,
    /// The pane's inside, at the last draw.
    inner: Rect,
}

impl Pane {
    fn new(lines: Vec<String>) -> Self {
        Self {
            lines,
            cursor: 0,
            scroll: ScrollViewState::default(),
            starts: Vec::new(),
            heights: Vec::new(),
            inner: Rect::default(),
        }
    }

    /// The cursor on line `to` (clamped to the last).
    fn cursor_to(&mut self, to: usize) {
        self.cursor = to.min(self.lines.len().saturating_sub(1));
    }

    /// The line drawn at `at`, by the last draw's rows.
    fn line_at(&self, at: Position) -> Option<usize> {
        if !self.inner.contains(at) {
            return None;
        }
        let row = self.scroll.offset().y + (at.y - self.inner.y);
        self.starts
            .iter()
            .zip(&self.heights)
            .position(|(start, height)| (*start..start.saturating_add(*height)).contains(&row))
    }

    /// Line `index` as drawn, the cursor's bar on it when focused.
    fn line(&self, index: usize, styles: &PaneStyles) -> Line<'static> {
        let mut line = Line::styled(self.lines[index].clone(), styles.text);
        if index == self.cursor
            && let Some(cursor) = styles.cursor
        {
            line = line.patch_style(cursor);
        }
        line
    }

    /// The lines wrapped to the pane in a tui-scrollview `ScrollView`, as
    /// the announcement's pane, the offset moved to keep the cursor's rows
    /// in sight. Each line's rows come from `Paragraph::line_count`
    /// (ratatui's `unstable-rendered-line-info`, ADR-001), measured again
    /// a column narrower when the scrollbar takes one.
    fn draw(&mut self, frame: &mut Frame<'_>, inner: Rect, styles: &PaneStyles) {
        self.inner = inner;
        let lines: Vec<Line<'static>> = (0..self.lines.len())
            .map(|i| self.line(i, styles))
            .collect();
        let mut width = inner.width;
        let mut heights = measure(&lines, width);
        let total = |heights: &[u16]| heights.iter().map(|h| u32::from(*h)).sum::<u32>();
        if total(&heights) > u32::from(inner.height) {
            width = width.saturating_sub(1);
            heights = measure(&lines, width);
        }
        let mut starts = Vec::with_capacity(heights.len());
        let mut row = 0u16;
        for height in &heights {
            starts.push(row);
            row = row.saturating_add(*height);
        }
        let height = row;
        if let (Some(start), Some(rows)) = (starts.get(self.cursor), heights.get(self.cursor)) {
            let mut offset = self.scroll.offset();
            // A line taller than the pane shows from its top, draw after
            // draw: moving to its last rows would send the next draw back
            // to its first (`start < offset`), the view jumping on every
            // redraw.
            if *start < offset.y || *rows > inner.height {
                offset.y = *start;
            } else if start.saturating_add(*rows) > offset.y.saturating_add(inner.height) {
                offset.y = start.saturating_add(*rows).saturating_sub(inner.height);
            }
            self.scroll.set_offset(offset);
        }
        self.starts = starts;
        self.heights = heights;
        split::render_scrolled(
            frame,
            inner,
            &mut self.scroll,
            Paragraph::new(lines).wrap(Wrap { trim: false }),
            Size::new(width, height),
        );
    }
}

/// Each line's rows when wrapped to `width`.
fn measure(lines: &[Line<'static>], width: u16) -> Vec<u16> {
    lines
        .iter()
        .map(|line| {
            let rows = Paragraph::new(line.clone())
                .wrap(Wrap { trim: false })
                .line_count(width);
            u16::try_from(rows.max(1)).unwrap_or(u16::MAX)
        })
        .collect()
}

/// A form field: one line, the passwords masked, no undo history (it
/// would keep copies of what was typed).
fn field(masked: bool) -> TextArea<'static> {
    let mut area = TextArea::default();
    area.set_max_histories(0);
    if masked {
        area.set_mask_char(MASK_CHAR);
    }
    area.set_cursor_line_style(Style::new());
    area
}

/// `path`'s lines as the viewer shows them: each made safe (term-ui's
/// `DisplayText`, colour off); a file that can't be read is one line
/// saying why.
fn read_lines(path: &Path) -> Vec<String> {
    match std::fs::read(path) {
        Ok(bytes) => DisplayText::from_bytes(&bytes, false)
            .lines()
            .map(str::to_owned)
            .collect(),
        Err(err) => vec![format!("can't read the file: {err}")],
    }
}

impl App for Screen {
    fn input(&mut self, input: Input) -> Flow {
        match input {
            Input::Key(key) if self.overlay.is_open() => self.overlay.key(key),
            Input::Mouse(mouse) if self.overlay.is_open() => self.overlay.mouse(mouse),
            Input::Key(key) if Overlay::opens(&key, self.mode == Mode::Form) => {
                self.overlay.open();
                Flow::Redraw
            }
            Input::Key(key) => match self.mode {
                Mode::Form => self.form_key(key),
                Mode::Run => Flow::Unchanged,
                Mode::Review => self.review_key(key),
            },
            Input::Mouse(mouse) => self.mouse(mouse),
            _ => Flow::Unchanged,
        }
    }

    /// The fetch's updates; the summary ends the run and opens the viewer.
    fn wake(&mut self) -> Flow {
        let mut changed = false;
        while let Ok(update) = self.updates.try_recv() {
            changed = true;
            match update {
                Update::Fetching(index) => {
                    if let Some(row) = self.rows.get_mut(index) {
                        row.status = Status::Fetching;
                    }
                }
                Update::Finished(index, status) => {
                    if let Some(row) = self.rows.get_mut(index) {
                        row.status = status;
                    }
                }
                Update::Summary(summary) => {
                    self.summary = Some(summary);
                    if self.mode == Mode::Run {
                        self.review();
                    }
                }
            }
        }
        if changed {
            Flow::Redraw
        } else {
            Flow::Unchanged
        }
    }

    fn draw(&mut self, frame: &mut Frame<'_>) {
        let [title, _, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let context = match self.mode {
            Mode::Form => "sign in to fetch your backed-up configuration",
            Mode::Run => "fetching your configuration files",
            Mode::Review => "check your configuration files",
        };
        frame.render_widget(Paragraph::new(self.title(context)), title);
        match self.mode {
            Mode::Form => self.draw_form(frame, body, footer),
            Mode::Run => self.draw_run(frame, body, footer),
            Mode::Review => self.draw_review(frame, body, footer),
        }
        let keys = match self.mode {
            Mode::Form => &FORM_KEYS,
            Mode::Run => &RUN_KEYS,
            Mode::Review => &REVIEW_KEYS,
        };
        self.overlay.draw(frame, keys, &self.theme);
    }
}

#[cfg(test)]
mod tests;
