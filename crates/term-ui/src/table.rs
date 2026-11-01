//! Tables in proto-inline's reviewed `docker ps` look (term-ui spec,
//! "Tables"): tabled's rounded style with a rule between rows, bold centred
//! headings, the border in the `Border` role, per-cell roles and
//! right-aligned columns from the caller, an optional maximum column
//! width, fitted to the terminal (the widest column wraps first) and of
//! natural width when stdout isn't one.
//! https://docs.rs/tabled/0.22.0/tabled/

use std::io::Write;

use sb_cli_core::exit::output_error;
use sb_cli_core::theme::{self, Role, Theme};
use tabled::builder::Builder;
use tabled::settings::formatting::{AlignmentStrategy, TrimStrategy};
use tabled::settings::object::{Columns, Rows, Segment};
use tabled::settings::peaker::Priority;
use tabled::settings::style::BorderColor;
use tabled::settings::{Alignment, Color, Style, Width};

use crate::style::table_colour;

/// One cell: its text and, optionally, the theme role it is drawn in.
#[derive(Clone, Debug)]
pub struct Cell {
    text: String,
    role: Option<Role>,
}

impl Cell {
    /// A cell of plain text, in the terminal's default colour.
    pub fn new(text: impl Into<String>) -> Self {
        Cell {
            text: text.into(),
            role: None,
        }
    }

    /// Draw this cell in `role`'s colour (when colour is on).
    pub fn role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        Cell::new(text)
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Cell::new(text)
    }
}

/// A table: headings, rows of cells, right-aligned columns and an optional
/// maximum column width (sb-go's `docker ps` caps at 100).
#[derive(Clone, Debug, Default)]
pub struct Table {
    headings: Vec<String>,
    rows: Vec<Vec<Cell>>,
    right: Vec<usize>,
    max_column_width: Option<usize>,
}

impl Table {
    /// A table with these headings and no rows yet.
    pub fn new<I, S>(headings: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Table {
            headings: headings.into_iter().map(Into::into).collect(),
            ..Table::default()
        }
    }

    /// Align column `index` (from 0) to the right.
    pub fn right_align(mut self, index: usize) -> Self {
        self.right.push(index);
        self
    }

    /// Wrap any column wider than `width` (at word boundaries).
    pub fn max_column_width(mut self, width: usize) -> Self {
        self.max_column_width = Some(width);
        self
    }

    /// Add a row, one cell per heading (tabled pads a short row and widens
    /// the table for a long one).
    pub fn row<I, C>(&mut self, cells: I)
    where
        I: IntoIterator<Item = C>,
        C: Into<Cell>,
    {
        self.rows.push(cells.into_iter().map(Into::into).collect());
    }

    /// The table as text: in `theme`'s colours when `colour` is on (none
    /// under `ascii`, bold included), fitted to `width` when given.
    pub fn render(&self, theme: &Theme, colour: bool, width: Option<usize>) -> String {
        let mut builder = Builder::default();
        builder.push_record(self.headings.clone());
        for row in &self.rows {
            builder.push_record(row.iter().map(|cell| cell.text.clone()));
        }
        let mut table = builder.build();
        // Each line of a wrapped cell aligned on its own (tabled aligns a
        // multi-line cell as one block by default), without the spaces
        // wrapping at words leaves at line ends.
        // https://docs.rs/tabled/0.22.0/tabled/settings/formatting/enum.AlignmentStrategy.html
        table
            .with(Style::modern_rounded())
            .modify(Segment::all(), AlignmentStrategy::PerLine)
            .modify(Segment::all(), TrimStrategy::Horizontal);
        for &index in &self.right {
            table.modify(Columns::one(index), Alignment::right());
        }
        // After the columns' alignment, so headings stay centred.
        table.modify(Rows::first(), Alignment::center());
        if let Some(max) = self.max_column_width {
            table.modify(Columns::new(..), Width::wrap(max).keep_words(true));
        }
        // The theme's colours, and bold headings only where the theme has
        // colour at all (every role has one, except under `ascii`): `ascii`
        // and colour off write no SGR codes.
        if let Some(border) = table_colour(theme, Role::Border, colour) {
            table.modify(Segment::all(), BorderColor::filled(border));
            table.modify(Rows::first(), Color::BOLD);
        }
        for (i, row) in self.rows.iter().enumerate() {
            for (j, cell) in row.iter().enumerate() {
                if let Some(paint) = cell.role.and_then(|role| table_colour(theme, role, colour)) {
                    table.modify((i + 1, j), paint);
                }
            }
        }
        // Fit the terminal: the widest column wraps first, at words.
        if let Some(width) = width {
            table.with(
                Width::wrap(width)
                    .priority(Priority::max(true))
                    .keep_words(true),
            );
        }
        table.to_string()
    }

    /// Print the table on stdout: fitted to the terminal there (console's
    /// `size_checked`, none when stdout isn't a terminal, so piped output
    /// keeps its natural width), in colour as stdout's choice says.
    pub fn print(&self) -> Result<(), sb_cli_core::exit::OutputError> {
        let width = console::Term::stdout()
            .size_checked()
            .map(|(_, cols)| usize::from(cols));
        let colour = crate::colour_on(crate::Stream::Stdout);
        let text = self.render(theme::current(), colour, width);
        writeln!(std::io::stdout(), "{text}").map_err(output_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::sgr::{colour_codes, of_kind};
    use sb_cli_core::theme::{Palette, Profile};

    /// A `docker ps`-like table, as proto-inline's reviewed one.
    fn containers() -> Table {
        let mut table = Table::new(["Container", "Status", "Traefik Port", "Port Bindings"])
            .right_align(2)
            .right_align(3);
        table.row([
            Cell::new("authelia"),
            Cell::new("running").role(Role::Ok),
            Cell::new("9091"),
            Cell::new(""),
        ]);
        table.row([
            Cell::new("plex"),
            Cell::new("running").role(Role::Ok),
            Cell::new("32400"),
            Cell::new("0.0.0.0:32400->32400/tcp"),
        ]);
        table.row([
            Cell::new("sonarr"),
            Cell::new("exited (1)").role(Role::Problem),
            Cell::new("8989"),
            Cell::new(""),
        ]);
        table
    }

    /// A wider one, nearer real data (several bindings, one per line), so
    /// 120 and 70 columns wrap too.
    fn wide() -> Table {
        let mut table = containers();
        table.row([
            Cell::new("a-container-with-a-very-long-descriptive-name"),
            Cell::new("running (healthy) for three days").role(Role::Ok),
            Cell::new("8080"),
            Cell::new("0.0.0.0:8080->8080/tcp\n[::]:8080->8080/tcp\n0.0.0.0:8443->8443/tcp"),
        ]);
        table
    }

    fn plain() -> Theme {
        Theme::resolve(&Palette::default(), Profile::Ascii)
    }

    /// The reviewed look, at its natural width.
    #[test]
    fn a_table_draws_the_reviewed_look() {
        insta::assert_snapshot!(containers().render(&plain(), false, None), @r"
        ╭───────────┬────────────┬──────────────┬──────────────────────────╮
        │ Container │   Status   │ Traefik Port │      Port Bindings       │
        ├───────────┼────────────┼──────────────┼──────────────────────────┤
        │ authelia  │ running    │         9091 │                          │
        ├───────────┼────────────┼──────────────┼──────────────────────────┤
        │ plex      │ running    │        32400 │ 0.0.0.0:32400->32400/tcp │
        ├───────────┼────────────┼──────────────┼──────────────────────────┤
        │ sonarr    │ exited (1) │         8989 │                          │
        ╰───────────┴────────────┴──────────────┴──────────────────────────╯
        ");
    }

    /// Fitted to 120, 70 and 50 columns: never wider, the widest column
    /// wrapping first.
    #[test]
    fn a_table_fits_the_terminal() {
        for width in [120, 70, 50] {
            // The wide sample is wider than each, so each really wraps.
            let natural = wide().render(&plain(), false, None);
            assert!(
                natural
                    .lines()
                    .any(|l| console::measure_text_width(l) > width)
            );
            let text = wide().render(&plain(), false, Some(width));
            for line in text.lines() {
                assert!(
                    console::measure_text_width(line) <= width,
                    "{width}: {line:?}"
                );
            }
        }
        insta::assert_snapshot!(containers().render(&plain(), false, Some(50)), @r"
        ╭───────────┬────────────┬───────────┬───────────╮
        │ Container │   Status   │  Traefik  │   Port    │
        │           │            │   Port    │ Bindings  │
        ├───────────┼────────────┼───────────┼───────────┤
        │ authelia  │ running    │      9091 │           │
        ├───────────┼────────────┼───────────┼───────────┤
        │ plex      │ running    │     32400 │ 0.0.0.0:3 │
        │           │            │           │ 2400->324 │
        │           │            │           │    00/tcp │
        ├───────────┼────────────┼───────────┼───────────┤
        │ sonarr    │ exited (1) │      8989 │           │
        ╰───────────┴────────────┴───────────┴───────────╯
        ");
    }

    /// A maximum column width wraps a long cell, at words.
    #[test]
    fn a_long_cell_wraps_at_the_maximum_width() {
        let mut table = Table::new(["Name", "Notes"]).max_column_width(10);
        table.row(["x", "one two three four"]);
        let text = table.render(&plain(), false, None);
        assert!(
            text.lines()
                .all(|l| console::measure_text_width(l) <= 1 + 6 + 1 + 12 + 1)
        );
        assert!(
            text.contains("│ one two    │") && text.contains("│ three four │"),
            "{text}"
        );
    }

    /// Colour off, or `ascii`: no escape codes at all, bold included.
    #[test]
    fn no_colour_writes_no_escape_codes() {
        let truecolor = Theme::resolve(&Palette::default(), Profile::TrueColor);
        assert!(
            !containers()
                .render(&truecolor, false, None)
                .contains('\x1b')
        );
        assert!(!containers().render(&plain(), true, None).contains('\x1b'));
    }

    /// With colour, every colour code is of the profile's kind, the border
    /// and the cells' roles included.
    #[test]
    fn table_colours_follow_the_profile() {
        for profile in [Profile::TrueColor, Profile::Ansi256, Profile::Ansi] {
            let theme = Theme::resolve(&Palette::default(), profile);
            let text = containers().render(&theme, true, None);
            let codes = colour_codes(&text);
            assert!(!codes.is_empty(), "{profile:?}");
            assert!(
                codes.iter().all(|c| of_kind(c, profile)),
                "{profile:?}: {codes:?}"
            );
            assert!(text.contains(&crate::style::paint(&theme, Role::Ok, true, "running")));
            assert!(text.contains(&crate::style::paint(
                &theme,
                Role::Problem,
                true,
                "exited (1)"
            )));
            // Headings bold with colour on.
            assert!(text.contains("\x1b[1mContainer"), "{profile:?}: {text:?}");
        }
    }
}
