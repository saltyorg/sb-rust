//! Prototype: fact editor on ratatui + ratatui-textarea, keyboard and mouse.
//! Throwaway; see FINDINGS.md. Never point it at the live facts directory.

mod model;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Parser;
use model::{FactId, Pending, Role};
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::TextArea;

#[derive(Parser)]
struct Cli {
    /// Directory of role .ini files (use a copy).
    dir: PathBuf,
}

#[derive(Clone, Copy, PartialEq)]
enum Row {
    Role(usize),
    Instance(usize, usize),
    Fact(FactId),
}

enum Mode {
    Browse,
    Edit(Box<TextArea<'static>>),
    Review,
    ConfirmQuit,
}

struct App {
    roles: Vec<Role>,
    expanded: BTreeSet<(usize, Option<usize>)>,
    list: ListState,
    pending: BTreeMap<FactId, Pending>,
    mode: Mode,
    reveal: bool,
    status: String,
    quit: bool,
    /// Areas from the last draw, used to map mouse positions without a
    /// hand-maintained hit-map.
    tree_area: Rect,
    menu_area: Rect,
    menu: Option<Menu>,
}

/// Right-click action menu for one fact.
struct Menu {
    target: FactId,
    at: Position,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if std::fs::canonicalize(&cli.dir)? == std::path::Path::new("/opt/saltbox") {
        bail!("refusing to edit the live facts directory; pass a copy");
    }
    let roles = model::load(&cli.dir)?;
    let mut app = App {
        roles,
        expanded: BTreeSet::new(),
        list: ListState::default().with_selected(Some(0)),
        pending: BTreeMap::new(),
        mode: Mode::Browse,
        reveal: false,
        status: String::from("Loaded"),
        quit: false,
        tree_area: Rect::default(),
        menu_area: Rect::default(),
        menu: None,
    };
    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    while !app.quit {
        terminal.draw(|f| draw(f, app))?;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key)?,
            Event::Mouse(mouse) => app.on_mouse(mouse),
            _ => {}
        }
    }
    Ok(())
}

impl App {
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (ri, role) in self.roles.iter().enumerate() {
            rows.push(Row::Role(ri));
            if !self.expanded.contains(&(ri, None)) {
                continue;
            }
            for (ii, inst) in role.instances.iter().enumerate() {
                rows.push(Row::Instance(ri, ii));
                if self.expanded.contains(&(ri, Some(ii))) {
                    rows.extend((0..inst.facts.len()).map(|fi| Row::Fact((ri, ii, fi))));
                }
            }
        }
        rows
    }

    fn selected(&self) -> Option<Row> {
        self.rows().get(self.list.selected()?).copied()
    }

    fn on_key(&mut self, key: KeyEvent) -> Result<()> {
        match &mut self.mode {
            Mode::Edit(textarea) => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Browse;
                    self.status = "Edit cancelled".into();
                }
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
                    let value = textarea.lines().join("\n");
                    if let Some(Row::Fact(id)) = self.selected() {
                        let original = &self.roles[id.0].instances[id.1].facts[id.2].value;
                        if &value == original {
                            self.pending.remove(&id);
                        } else {
                            self.pending.insert(id, Pending::Set(value));
                        }
                    }
                    self.mode = Mode::Browse;
                    self.status = "Change staged".into();
                }
                _ => {
                    textarea.input(key);
                }
            },
            Mode::Review => match key.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    let written = model::apply(&self.roles, &self.pending)?;
                    self.roles =
                        model::load(self.roles[0].path.parent().expect("role file has a parent"))?;
                    self.status = format!(
                        "Applied {} change(s) to {} file(s)",
                        self.pending.len(),
                        written.len()
                    );
                    self.pending.clear();
                    self.mode = Mode::Browse;
                }
                KeyCode::Esc | KeyCode::Char('n') => self.mode = Mode::Browse,
                _ => {}
            },
            Mode::ConfirmQuit => match key.code {
                KeyCode::Char('y') => self.quit = true,
                _ => self.mode = Mode::Browse,
            },
            Mode::Browse => self.on_browse_key(key),
        }
        Ok(())
    }

    fn on_browse_key(&mut self, key: KeyEvent) {
        let rows = self.rows();
        let sel = self.list.selected().unwrap_or(0);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self
                .list
                .select(Some((sel + 1).min(rows.len().saturating_sub(1)))),
            KeyCode::Up | KeyCode::Char('k') => self.list.select(Some(sel.saturating_sub(1))),
            KeyCode::Home | KeyCode::Char('g') => self.list.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') => {
                self.list.select(Some(rows.len().saturating_sub(1)))
            }
            KeyCode::Right | KeyCode::Char('l') => match rows.get(sel) {
                Some(Row::Role(r)) => {
                    self.expanded.insert((*r, None));
                }
                Some(Row::Instance(r, i)) => {
                    self.expanded.insert((*r, Some(*i)));
                }
                _ => {}
            },
            KeyCode::Left | KeyCode::Char('h') => self.collapse_or_parent(&rows, sel),
            KeyCode::Enter => match rows.get(sel) {
                Some(Row::Role(r)) => toggle(&mut self.expanded, (*r, None)),
                Some(Row::Instance(r, i)) => toggle(&mut self.expanded, (*r, Some(*i))),
                Some(Row::Fact(id)) => self.start_edit(*id),
                None => {}
            },
            KeyCode::Char('e') => {
                if let Some(Row::Fact(id)) = rows.get(sel) {
                    self.start_edit(*id);
                }
            }
            KeyCode::Char('d') => {
                if let Some(Row::Fact(id)) = rows.get(sel) {
                    if matches!(self.pending.get(id), Some(Pending::Delete)) {
                        self.pending.remove(id);
                        self.status = "Delete undone".into();
                    } else {
                        self.pending.insert(*id, Pending::Delete);
                        self.status = "Delete staged".into();
                    }
                }
            }
            KeyCode::Char('u') => {
                if let Some(Row::Fact(id)) = rows.get(sel) {
                    self.pending.remove(id);
                    self.status = "Change undone".into();
                }
            }
            KeyCode::Char('r') => self.reveal = !self.reveal,
            KeyCode::Char('a') if !self.pending.is_empty() => self.mode = Mode::Review,
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.pending.is_empty() {
                    self.quit = true;
                } else {
                    self.mode = Mode::ConfirmQuit;
                }
            }
            _ => {}
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        if !matches!(self.mode, Mode::Browse) {
            return;
        }
        let pos = Position::new(m.column, m.row);
        if let Some(menu) = self.menu.take() {
            // Any click closes the menu; a left click on an item also runs it.
            if m.kind == MouseEventKind::Down(MouseButton::Left)
                && self.menu_area.contains(pos)
                && m.row > self.menu_area.y
                && let Some((_, code)) =
                    menu_items(self, menu.target).get(usize::from(m.row - self.menu_area.y - 1))
            {
                self.on_browse_key(KeyEvent::from(*code));
            }
            if !matches!(m.kind, MouseEventKind::Down(_)) {
                self.menu = Some(menu); // keep open on moves/scrolls
            }
            return;
        }
        if !self.tree_area.contains(pos) {
            return;
        }
        let rows = self.rows();
        let hit =
            tree_row_at(self.tree_area, self.list.offset(), m.row).filter(|i| *i < rows.len());
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(i) = hit {
                    if self.list.selected() == Some(i) {
                        // Second click on the selected row acts like Enter.
                        self.on_browse_key(KeyEvent::from(KeyCode::Enter));
                    } else {
                        self.list.select(Some(i));
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(i) = hit {
                    self.list.select(Some(i));
                    if let Row::Fact(id) = rows[i] {
                        self.menu = Some(Menu {
                            target: id,
                            at: pos,
                        });
                    }
                }
            }
            MouseEventKind::ScrollDown => self.on_browse_key(KeyEvent::from(KeyCode::Down)),
            MouseEventKind::ScrollUp => self.on_browse_key(KeyEvent::from(KeyCode::Up)),
            _ => {}
        }
    }

    fn collapse_or_parent(&mut self, rows: &[Row], sel: usize) {
        let target = match rows.get(sel) {
            Some(Row::Role(r)) => {
                self.expanded.remove(&(*r, None));
                None
            }
            Some(Row::Instance(r, i)) if self.expanded.remove(&(*r, Some(*i))) => None,
            Some(Row::Instance(r, _)) => Some(Row::Role(*r)),
            Some(Row::Fact((r, i, _))) => Some(Row::Instance(*r, *i)),
            None => None,
        };
        if let Some(t) = target {
            self.list.select(rows.iter().position(|row| *row == t));
        }
    }

    fn start_edit(&mut self, id: FactId) {
        let current = match self.pending.get(&id) {
            Some(Pending::Set(v)) => v.clone(),
            _ => self.roles[id.0].instances[id.1].facts[id.2].value.clone(),
        };
        let mut textarea = TextArea::new(current.lines().map(String::from).collect());
        textarea.set_block(
            Block::bordered()
                .title(" Edit value · Enter save · Esc cancel ")
                .border_type(BorderType::Rounded),
        );
        textarea.set_cursor_line_style(Style::default());
        textarea.move_cursor(ratatui_textarea::CursorMove::Bottom);
        textarea.move_cursor(ratatui_textarea::CursorMove::End);
        self.mode = Mode::Edit(Box::new(textarea));
        self.status = "Editing".into();
    }

    fn display_value(&self, id: FactId) -> String {
        let fact = &self.roles[id.0].instances[id.1].facts[id.2];
        if self.reveal || !model::looks_secret(&fact.key) {
            fact.value.clone()
        } else {
            "•".repeat(fact.value.chars().count().clamp(4, 12))
        }
    }
}

fn toggle(set: &mut BTreeSet<(usize, Option<usize>)>, key: (usize, Option<usize>)) {
    if !set.remove(&key) {
        set.insert(key);
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let facts: usize = app
        .roles
        .iter()
        .flat_map(|r| &r.instances)
        .map(|i| i.facts.len())
        .sum();
    f.render_widget(
        Line::from(vec![
            " ◆ Saltbox Facts ".bold().cyan(),
            Span::raw(format!(" {} roles · {facts} facts ", app.roles.len())).dim(),
            if app.pending.is_empty() {
                Span::raw("")
            } else {
                format!(" {} pending ", app.pending.len())
                    .black()
                    .on_yellow()
            },
        ]),
        header,
    );

    let wide = body.width >= 96;
    let [tree_area, inspector_area] = if wide {
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(body)
    } else {
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body)
    };

    let items: Vec<ListItem> = app.rows().iter().map(|row| tree_item(app, *row)).collect();
    let tree = List::new(items)
        .block(
            Block::bordered()
                .title(" Fact tree ")
                .border_type(BorderType::Rounded),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ");
    f.render_stateful_widget(tree, tree_area, &mut app.list);
    app.tree_area = tree_area;

    f.render_widget(
        inspector(app).block(
            Block::bordered()
                .title(" Inspector ")
                .border_type(BorderType::Rounded),
        ),
        inspector_area,
    );
    if let Mode::Edit(textarea) = &app.mode {
        // Modal editor: a fixed share of the screen, independent of the inspector size.
        let [area] = Layout::vertical([Constraint::Percentage(50)])
            .flex(Flex::Center)
            .areas(f.area());
        let [area] = Layout::horizontal([Constraint::Percentage(80)])
            .flex(Flex::Center)
            .areas(area);
        f.render_widget(Clear, area);
        f.render_widget(&**textarea, area);
    }

    let hints = match app.mode {
        Mode::Browse => {
            "↑↓ move  →/← expand  Enter edit  d delete  u undo  r reveal  a apply  q quit  · click select, right-click actions"
        }
        Mode::Edit(_) => "Enter save  Alt+Enter newline  Esc cancel",
        Mode::Review => "Enter/y apply  Esc back",
        Mode::ConfirmQuit => "y discard and quit  any other key to go back",
    };
    f.render_widget(
        Line::from(vec![
            format!(" {} ", app.status).reversed(),
            Span::raw(" "),
            hints.dim(),
        ]),
        footer,
    );

    match app.mode {
        Mode::Review => popup(f, " Review changes ", review_lines(app)),
        Mode::ConfirmQuit => popup(
            f,
            " Unsaved changes ",
            vec![Line::from(format!(
                "{} change(s) not applied. Quit anyway? (y/N)",
                app.pending.len()
            ))],
        ),
        _ => {}
    }

    if let Some(menu) = &app.menu {
        let items = menu_items(app, menu.target);
        let width = 18u16.min(f.area().width);
        let height = (u16::try_from(items.len()).unwrap_or(0) + 2).min(f.area().height);
        // Open at the click, shifted back inside the screen if it would overflow.
        let x = menu.at.x.min(f.area().right().saturating_sub(width));
        let y = menu.at.y.min(f.area().bottom().saturating_sub(height));
        let area = Rect::new(x, y, width, height);
        app.menu_area = area;
        let lines: Vec<Line> = items
            .iter()
            .map(|(label, _)| Line::from(format!(" {label}")))
            .collect();
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(lines).block(Block::bordered().border_type(BorderType::Rounded)),
            area,
        );
    }
}

/// Menu entries map to the same keys the keyboard uses, so behaviour can't drift.
fn menu_items(app: &App, id: FactId) -> Vec<(&'static str, KeyCode)> {
    let mut items = vec![("Edit", KeyCode::Char('e'))];
    match app.pending.get(&id) {
        Some(Pending::Delete) => items.push(("Undo delete", KeyCode::Char('d'))),
        Some(Pending::Set(_)) => {
            items.push(("Delete", KeyCode::Char('d')));
            items.push(("Undo change", KeyCode::Char('u')));
        }
        None => items.push(("Delete", KeyCode::Char('d'))),
    }
    items
}

fn tree_item(app: &App, row: Row) -> ListItem<'static> {
    let arrow = |open: bool| if open { "▾ " } else { "▸ " };
    match row {
        Row::Role(r) => {
            let role = &app.roles[r];
            let dirty = app.pending.keys().any(|(pr, _, _)| *pr == r);
            ListItem::new(Line::from(vec![
                Span::raw(arrow(app.expanded.contains(&(r, None)))),
                Span::styled(role.name.clone(), Style::default().bold()),
                Span::raw(format!(" ({})", role.instances.len())).dim(),
                if dirty {
                    " ●".yellow()
                } else {
                    Span::raw("")
                },
            ]))
        }
        Row::Instance(r, i) => ListItem::new(Line::from(vec![
            Span::raw("  "),
            Span::raw(arrow(app.expanded.contains(&(r, Some(i))))),
            Span::raw(app.roles[r].instances[i].name.clone()).blue(),
        ])),
        Row::Fact(id) => {
            let key = app.roles[id.0].instances[id.1].facts[id.2].key.clone();
            let (marker, style) = match app.pending.get(&id) {
                Some(Pending::Set(_)) => (" ●", Style::default().fg(Color::Yellow)),
                Some(Pending::Delete) => (
                    " ✗",
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::CROSSED_OUT),
                ),
                None => ("", Style::default()),
            };
            ListItem::new(Line::from(vec![
                Span::raw("      "),
                Span::styled(key, style),
                Span::styled(marker, style),
            ]))
        }
    }
}

fn inspector(app: &App) -> Paragraph<'static> {
    let label = |s: &str| Span::raw(format!("{s:<10}")).dim();
    let lines = match app.selected() {
        Some(Row::Role(r)) => {
            let role = &app.roles[r];
            vec![
                Line::from(vec![label("Role"), role.name.clone().bold()]),
                Line::from(vec![
                    label("Instances"),
                    Span::raw(role.instances.len().to_string()),
                ]),
            ]
        }
        Some(Row::Instance(r, i)) => vec![
            Line::from(vec![label("Role"), Span::raw(app.roles[r].name.clone())]),
            Line::from(vec![
                label("Instance"),
                app.roles[r].instances[i].name.clone().bold(),
            ]),
            Line::from(vec![
                label("Facts"),
                Span::raw(app.roles[r].instances[i].facts.len().to_string()),
            ]),
        ],
        Some(Row::Fact(id)) => {
            let fact = &app.roles[id.0].instances[id.1].facts[id.2];
            let mut lines = vec![
                Line::from(vec![label("Role"), Span::raw(app.roles[id.0].name.clone())]),
                Line::from(vec![
                    label("Instance"),
                    Span::raw(app.roles[id.0].instances[id.1].name.clone()),
                ]),
                Line::from(vec![label("Key"), fact.key.clone().bold()]),
            ];
            lines.extend(value_lines(
                label("Value"),
                app.display_value(id),
                Style::default(),
            ));
            match app.pending.get(&id) {
                Some(Pending::Set(v)) => {
                    let shown = if app.reveal || !model::looks_secret(&fact.key) {
                        v.clone()
                    } else {
                        "•".repeat(8)
                    };
                    lines.extend(value_lines(
                        label("Pending"),
                        shown,
                        Style::default().fg(Color::Yellow),
                    ));
                }
                Some(Pending::Delete) => {
                    lines.push(Line::from(vec![label("Pending"), "delete".red()]))
                }
                None => {}
            }
            lines
        }
        None => vec![Line::from("No facts found").dim()],
    };
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

/// ratatui Spans do not break on '\n'; emit one Line per value line.
fn value_lines(label: Span<'static>, value: String, style: Style) -> Vec<Line<'static>> {
    let pad = Span::raw(" ".repeat(label.width()));
    value
        .lines()
        .enumerate()
        .map(|(i, l)| {
            let lead = if i == 0 { label.clone() } else { pad.clone() };
            Line::from(vec![lead, Span::styled(l.to_string(), style)])
        })
        .collect()
}

fn review_lines(app: &App) -> Vec<Line<'static>> {
    app.pending
        .iter()
        .map(|((r, i, fi), p)| {
            let role = &app.roles[*r];
            let key = &role.instances[*i].facts[*fi].key;
            let what = match p {
                Pending::Set(_) => "set".yellow(),
                Pending::Delete => "delete".red(),
            };
            Line::from(vec![
                what,
                Span::raw(format!("  {}/{}/{key}", role.name, role.instances[*i].name)),
            ])
        })
        .collect()
}

fn popup(f: &mut Frame, title: &'static str, lines: Vec<Line<'static>>) {
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(f.area().height);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(f.area());
    let [area] = Layout::horizontal([Constraint::Percentage(70)])
        .flex(Flex::Center)
        .areas(area);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .title(title),
        ),
        area,
    );
}

/// Maps a terminal row inside the bordered tree to a list index.
fn tree_row_at(area: Rect, offset: usize, y: u16) -> Option<usize> {
    (y > area.y && y < area.bottom().saturating_sub(1))
        .then(|| offset + usize::from(y - area.y - 1))
}
