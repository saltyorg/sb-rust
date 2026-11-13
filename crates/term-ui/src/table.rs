//! Tables in proto-inline's reviewed `docker ps` look (term-ui spec,
//! "Tables"): tabled's rounded style with a rule between rows, bold centred
//! headings, the border in the `Border` role, per-cell roles and
//! right-aligned columns from the caller, an optional maximum column
//! width, fitted to the terminal (the widest column wraps first, each
//! cell's lines at words, or at any character in a column the caller
//! marks) and of natural width when stdout isn't one. A terminal too
//! narrow for a character a cell gets a line saying so instead.
//! <https://docs.rs/tabled/0.22.0/tabled/>

use std::io::Write;

use sb_cli_core::exit::output_error;
use sb_cli_core::theme::{self, Role, Theme};
use tabled::builder::Builder;
use tabled::grid::dimension::Estimate;
use tabled::settings::formatting::{AlignmentStrategy, TrimStrategy};
use tabled::settings::object::{Columns, Rows, Segment};
use tabled::settings::peaker::Priority;
use tabled::settings::style::BorderColor;
use tabled::settings::{Alignment, Color, Style, Width};

use crate::DisplayText;
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
    #[must_use]
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
    anywhere: Vec<usize>,
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
    #[must_use]
    pub fn right_align(mut self, index: usize) -> Self {
        self.right.push(index);
        self
    }

    /// Wrap column `index`'s cells at any character rather than at spaces
    /// (its heading still wraps at words): for text whose spaces are
    /// padding, such as `docker ps`'s aligned bindings.
    #[must_use]
    pub fn wrap_anywhere(mut self, index: usize) -> Self {
        self.anywhere.push(index);
        self
    }

    /// Wrap any column wider than `width` (at word boundaries).
    #[must_use]
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
    /// under `ascii`, bold included), fitted to `width` when given. Below
    /// the narrowest table that width can hold (a character a cell), a
    /// line saying so instead.
    #[must_use]
    pub fn render(&self, theme: &Theme, colour: bool, width: Option<usize>) -> String {
        // Every heading and cell is sanitized here, with its own colour
        // dropped (term-ui spec, "Escape handling"): a container label is
        // untrusted text. The table's colour is added after, by tabled.
        let headings: Vec<String> = self.headings.iter().map(|text| shown(text)).collect();
        let rows: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|row| row.iter().map(|cell| shown(&cell.text)).collect())
            .collect();
        let widths = match self.widths(&headings, &rows, width) {
            Ok(widths) => widths,
            Err(too_narrow) => return too_narrow,
        };
        // Each cell wrapped to its column here, a line at a time, then
        // drawn at those widths (why not tabled's own wrap: `wrapped`).
        // Every column has a width: they come from the same cells.
        let fit = |text: &str, column: usize, words: bool| {
            widths.get(column).map_or_else(
                || text.to_owned(),
                |&width| wrapped(text, width.saturating_sub(PADDING), words),
            )
        };
        let mut builder = Builder::default();
        builder.push_record(
            headings
                .iter()
                .enumerate()
                .map(|(j, text)| fit(text, j, true)),
        );
        for row in &rows {
            builder.push_record(
                row.iter()
                    .enumerate()
                    .map(|(j, text)| fit(text, j, !self.anywhere.contains(&j))),
            );
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
        // The cells already fit these widths; a column whose cells wrapped
        // at words keeps the width the fit gave it.
        // https://docs.rs/tabled/0.22.0/tabled/settings/width/struct.Width.html#method.list
        table.with(Width::list(widths));
        table.to_string()
    }

    /// The columns' widths, padding included: their natural widths, capped
    /// at the maximum column width, then fitted to `width` with the widest
    /// column narrowed first, as tabled's `Wrap` chooses them
    /// (<https://docs.rs/tabled/0.22.0/tabled/settings/width/struct.Wrap.html#method.priority>,
    /// `Priority::max`). `Err` is the line saying so, when `width` is below
    /// the narrowest table.
    fn widths(
        &self,
        headings: &[String],
        rows: &[Vec<String>],
        width: Option<usize>,
    ) -> Result<Vec<usize>, String> {
        let mut builder = Builder::default();
        builder.push_record(headings.iter().cloned());
        for row in rows {
            builder.push_record(row.iter().cloned());
        }
        let mut probe = builder.build();
        probe.with(Style::modern_rounded());
        if let Some(max) = self.max_column_width {
            probe.modify(Columns::new(..), Width::wrap(max));
        }
        if let Some(width) = width {
            let needed = narrowest(probe.count_columns());
            if width < needed {
                return Err(format!(
                    "Too narrow for the table: it needs {needed} columns, the terminal has {width}."
                ));
            }
            probe.with(Width::wrap(width).priority(Priority::max(true)));
        }
        // The widths tabled draws at, estimated as `Table::total_width`
        // does when nothing set them.
        let mut dimension = probe.get_dimension().clone();
        dimension.estimate(probe.get_records(), probe.get_config());
        Ok(dimension
            .get_widths()
            .map(<[usize]>::to_vec)
            .unwrap_or_default())
    }

    /// Print the table on stdout: fitted to the terminal there (console's
    /// `size_checked`, none when stdout isn't a terminal, so piped output
    /// keeps its natural width), in colour as stdout's choice says.
    ///
    /// # Errors
    ///
    /// The [`OutputError`](sb_cli_core::exit::OutputError) when stdout
    /// can't be written (its reader went away, say).
    pub fn print(&self) -> Result<(), sb_cli_core::exit::OutputError> {
        let width = console::Term::stdout()
            .size_checked()
            .map(|(_, cols)| usize::from(cols));
        let colour = crate::colour_on(crate::Stream::Stdout);
        let text = self.render(theme::current(), colour, width);
        writeln!(std::io::stdout(), "{text}").map_err(output_error)
    }
}

/// A cell's padding: tabled's default, a space on either side.
const PADDING: usize = 2;

/// The width of the narrowest table of `columns` columns in this style: a
/// character a cell, measured by tabled. A wide character (CJK, emoji)
/// needs two; in a one-character column tabled draws it as `�`.
fn narrowest(columns: usize) -> usize {
    let mut builder = Builder::default();
    builder.push_record(vec!["x"; columns]);
    builder.build().with(Style::modern_rounded()).total_width()
}

/// `text` wrapped to `width` columns: each of its lines on its own,
/// through [`crate::text::wrap_line`] (tabled's `Wrap::wrap`), at words
/// when `words` is set, else at any character. A piece that held only
/// spaces (the space after a full line, or a run of padding) is dropped;
/// a blank line of the cell's own is kept.
///
/// Why not tabled's `Width::wrap(..).keep_words(true)` on the table
/// (ui-L3, tabled 0.22.0, its `wrap_text_keeping_words`, which hands
/// text without escapes to `wrap_text_keeping_words_noansi`): it reads
/// a cell's `\n` as a character of a word, so after a line break the next
/// line goes on counting from the last one; it moves the space after a
/// full line to the next line, leaving a blank one when the next word
/// doesn't fit beside it; it takes a binding's padding for word breaks;
/// and below a character a cell it empties cells, a column at a time.
fn wrapped(text: &str, width: usize, words: bool) -> String {
    let mut lines = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            lines.push(line.to_owned());
            continue;
        }
        lines.extend(crate::text::wrap_line(line, width, words));
    }
    lines.join("\n")
}

/// `text` as a cell shows it: [`DisplayText`] without colour, its lines
/// joined by `\n` again, which tabled splits a cell's lines at. Why not
/// tabled's `Charset::clean()`: it drops only the ESC byte and other C0,
/// leaving sequence payloads, DEL and C1.
fn shown(text: &str) -> String {
    DisplayText::from_bytes(text.as_bytes(), false)
        .as_str()
        .to_owned()
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

    /// A wrapped line that ends at the column's width is followed by the
    /// next words, not by a blank line: a space after a full line, and a
    /// cell's own line break after one (ui-L3).
    #[test]
    fn a_wrapped_cell_has_no_blank_line_at_the_width() {
        let mut table = Table::new(["Status"]);
        table.row(["created (failed to start)"]);
        table.row(["abcdef\nghijkl"]);
        // A blank line of the cell's own stays, wrapped or not.
        table.row(["a\n\nb"]);
        let mut own = Table::new(["Status"]);
        own.row(["a\n\nb"]);
        let text = own.render(&plain(), false, None);
        assert!(
            text.contains("│ a      │\n│        │\n│ b      │"),
            "{text}"
        );
        insta::assert_snapshot!(table.render(&plain(), false, Some(10)), @r"
        ╭────────╮
        │ Status │
        ├────────┤
        │ create │
        │ d (fai │
        │ led to │
        │ start) │
        ├────────┤
        │ abcdef │
        │ ghijkl │
        ├────────┤
        │ a      │
        │        │
        │ b      │
        ╰────────╯
        ");
    }

    /// A padded binding wraps as one piece of text, its padding kept,
    /// and each binding starts its own line (ui-L3). Headings still wrap
    /// at words.
    #[test]
    fn a_padded_binding_stays_whole() {
        let mut table = Table::new(["Port Bindings"])
            .right_align(0)
            .wrap_anywhere(0);
        table.row(["0.0.0.0: 8080->   80/tcp\n[::1]: 8080->   80/tcp"]);
        insta::assert_snapshot!(table.render(&plain(), false, Some(16)), @r"
        ╭──────────────╮
        │     Port     │
        │   Bindings   │
        ├──────────────┤
        │ 0.0.0.0: 808 │
        │ 0->   80/tcp │
        │ [::1]: 8080- │
        │   >   80/tcp │
        ╰──────────────╯
        ");
    }

    /// Below the narrowest table (one character a cell), the table says
    /// so instead of drawing empty cells; at that width, every cell shows
    /// a character (ui-L3).
    #[test]
    fn a_terminal_below_the_minimum_says_so() {
        insta::assert_snapshot!(
            containers().render(&plain(), false, Some(16)),
            @"Too narrow for the table: it needs 17 columns, the terminal has 16."
        );
        let narrowest = containers().render(&plain(), false, Some(17));
        // The heading line: every body row has an empty binding cell.
        let heading = narrowest.lines().nth(1).expect("a heading line");
        assert!(
            heading
                .split('│')
                .filter(|cell| !cell.is_empty())
                .all(|cell| !cell.trim().is_empty()),
            "{narrowest}"
        );
        assert!(
            narrowest
                .lines()
                .all(|l| console::measure_text_width(l) <= 17),
            "{narrowest}"
        );
    }

    /// A cell made from a `String` or a `&str` holds its text, no role.
    #[test]
    fn cells_convert_from_text() {
        for cell in [Cell::from("a"), Cell::from(String::from("a"))] {
            assert_eq!(cell.text, "a");
            assert!(cell.role.is_none());
        }
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
