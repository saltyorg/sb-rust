//! Tables in proto-inline's reviewed `docker ps` look (term-ui spec,
//! "Tables"): tabled's rounded style with a rule between rows, bold centred
//! headings, the border in the `Border` role, per-cell roles and
//! right-aligned columns from the caller, an optional maximum column
//! width, fitted to the terminal (the widest column wraps first, each
//! cell's lines at words, or at any character in a column the caller
//! marks) and of natural width when stdout isn't one. A terminal too
//! narrow for a character a cell gets a line saying so instead. Rows
//! across every column, bold (`sb list`'s later repositories, a lone
//! heading over several columns, and a title above the headings, as
//! bench's Speedtest table had), through tabled's `Span`. And a ruled
//! look, `sb bench`'s: no border, no rules, a two-space gap between
//! columns, muted headings aligned as their columns, minimum column
//! widths and bold cells (term-ui spec, "Tables", the ruled look).
//! <https://docs.rs/tabled/0.22.0/tabled/>

use std::io::Write;

use sb_cli_core::exit::output_error;
use sb_cli_core::theme::{self, Role, Theme};
use tabled::builder::Builder;
use tabled::grid::dimension::Estimate;
use tabled::settings::formatting::{AlignmentStrategy, TrimStrategy};
use tabled::settings::object::{Columns, Rows, Segment};
use tabled::settings::peaker::Priority;
use tabled::settings::span::ColumnSpan;
use tabled::settings::style::BorderColor;
use tabled::settings::themes::BorderCorrection;
use tabled::settings::{Alignment, Color, Padding, Style, Width};

use crate::DisplayText;
use crate::style::{table_colour, table_style};

/// One cell: its text and, optionally, the theme role it is drawn in.
#[derive(Clone, Debug)]
pub struct Cell {
    text: String,
    role: Option<Role>,
    bold: bool,
}

impl Cell {
    /// A cell of plain text, in the terminal's default colour.
    pub fn new(text: impl Into<String>) -> Self {
        Cell {
            text: text.into(),
            role: None,
            bold: false,
        }
    }

    /// Draw this cell in `role`'s colour (when colour is on).
    #[must_use]
    pub fn role(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Draw this cell bold (when colour is on), in its role's colour if
    /// it has one (bench's `Average`).
    #[must_use]
    pub fn bold(mut self) -> Self {
        self.bold = true;
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

/// A table: an optional title, headings, rows of cells, right-aligned
/// columns and an optional maximum column width (sb-go's `docker ps` caps
/// at 100), and rows that span every column (`sb list`).
#[derive(Clone, Debug, Default)]
pub struct Table {
    /// A row across every column above the headings (`Table::titled`).
    title: Option<String>,
    headings: Vec<String>,
    rows: Vec<Vec<Cell>>,
    /// The rows (by index in `rows`) that span every column.
    spanning: Vec<usize>,
    /// Whether the one heading spans every column.
    spanning_heading: bool,
    right: Vec<usize>,
    anywhere: Vec<usize>,
    max_column_width: Option<usize>,
    /// The ruled look ([`Table::ruled`]) rather than the boxed one.
    ruled: bool,
    /// Each column's minimum width, its gap included: (column, width).
    min_widths: Vec<(usize, usize)>,
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

    /// A table in the ruled look (`sb bench`'s): no border, no rule
    /// between rows, a two-space gap after each left-aligned column but
    /// the last and before each right-aligned column but the first (and
    /// before a left-aligned column after a right-aligned one), these
    /// headings in Muted and aligned as their columns (no heading row when
    /// there are none), and no line ending in a space. tabled's
    /// `Style::empty()`
    /// (<https://docs.rs/tabled/0.22.0/tabled/settings/style/struct.Style.html#method.empty>)
    /// and `Padding`
    /// (<https://docs.rs/tabled/0.22.0/tabled/settings/struct.Padding.html>).
    pub fn ruled<I, S>(headings: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Table {
            ruled: true,
            ..Table::new(headings)
        }
    }

    /// Make column `index` at least `width` wide, its gap included
    /// (bench's 21-wide label column), through tabled's `Width::increase`
    /// (<https://docs.rs/tabled/0.22.0/tabled/settings/width/struct.Width.html#method.increase>),
    /// which widens a cell's text, its padding aside.
    #[must_use]
    pub fn min_width(mut self, index: usize, width: usize) -> Self {
        self.min_widths.push((index, width));
        self
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

    /// A table whose one heading spans every column, bold and centred
    /// (`sb list`'s first repository, over the tag and its `sb install`).
    pub fn spanning(heading: impl Into<String>) -> Self {
        Table {
            headings: vec![heading.into()],
            spanning_heading: true,
            ..Table::default()
        }
    }

    /// A table with a title row across every column, then a row of these
    /// headings, both bold and centred, and no rows yet (bench's Speedtest
    /// table, over `Node | Upload | Download | Latency`). The title wraps
    /// across the whole table when it is fitted, and widens the columns
    /// when it is the widest record.
    pub fn titled<I, S>(title: impl Into<String>, headings: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Table {
            title: Some(title.into()),
            ..Table::new(headings)
        }
    }

    /// Add a row of `text` across every column, bold and centred as a
    /// heading (`sb list`'s later repositories: term-ui spec, "Tables",
    /// amended by the ansible spec), through tabled's `Span`
    /// (<https://docs.rs/tabled/0.22.0/tabled/settings/span/struct.Span.html>).
    pub fn spanning_row(&mut self, text: impl Into<String>) {
        self.spanning.push(self.rows.len());
        self.rows.push(vec![Cell::new(text)]);
    }

    /// How many records come before the rows: the headings (none for a
    /// ruled table without any), and the title above them when there is
    /// one.
    fn heading_rows(&self) -> usize {
        usize::from(!(self.ruled && self.headings.is_empty())) + usize::from(self.title.is_some())
    }

    /// The padding of column `column` of `columns`: tabled's default, a
    /// space on either side, in the boxed look; in the ruled look the
    /// two-space gap where [`Table::ruled`] puts it (before a left-aligned
    /// column after a right-aligned one too), and none otherwise.
    fn padding(&self, column: usize, columns: usize) -> (usize, usize) {
        if !self.ruled {
            return (1, 1);
        }
        let right = |column: usize| self.right.contains(&column);
        if right(column) {
            (if column > 0 { GAP } else { 0 }, 0)
        } else {
            let left = if column > 0 && right(column - 1) {
                GAP
            } else {
                0
            };
            (left, if column + 1 < columns { GAP } else { 0 })
        }
    }

    /// `table` in this table's look: its style and each column's padding.
    fn look(&self, table: &mut tabled::Table) {
        if !self.ruled {
            table.with(Style::modern_rounded());
            return;
        }
        table.with(Style::empty());
        let columns = table.count_columns();
        for column in 0..columns {
            let (left, right) = self.padding(column, columns);
            table.modify(Columns::one(column), Padding::new(left, right, 0, 0));
        }
    }

    /// The records (the first is 0) that span every column: the title of
    /// a [`Table::titled`], the heading row of a [`Table::spanning`], and
    /// each spanning row.
    fn spans(&self) -> Vec<usize> {
        let head = self.heading_rows();
        (self.title.is_some() || self.spanning_heading)
            .then_some(0)
            .into_iter()
            .chain(self.spanning.iter().map(|row| row + head))
            .collect()
    }

    /// The table as text: in `theme`'s colours when `colour` is on (none
    /// under `ascii`, bold included), fitted to `width` when given. Below
    /// the narrowest table that width can hold (a character a cell), a
    /// line saying so instead.
    #[must_use]
    pub fn render(&self, theme: &Theme, colour: bool, width: Option<usize>) -> String {
        // Every title, heading and cell is sanitized here, with its own colour
        // dropped (term-ui spec, "Escape handling"): a container label is
        // untrusted text. The table's colour is added after, by tabled.
        let title = self.title.as_deref().map(shown);
        let headings: Vec<String> = self.headings.iter().map(|text| shown(text)).collect();
        let rows: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|row| row.iter().map(|cell| shown(&cell.text)).collect())
            .collect();
        let spans = self.spans();
        let head = self.heading_rows();
        let (widths, inner) = match self.widths(title.as_deref(), &headings, &rows, &spans, width) {
            Ok(fitted) => fitted,
            Err(too_narrow) => return too_narrow,
        };
        // Each cell wrapped to its column here, a line at a time, then
        // drawn at those widths (why not tabled's own wrap: `wrapped`).
        // Every column has a width: they come from the same cells. A
        // spanning cell has every column's width and the borders between
        // them, as tabled counts them.
        let across = widths.iter().sum::<usize>() + inner;
        let columns = widths.len();
        let fit = |text: &str, record: usize, column: usize, words: bool| {
            let (width, padded) = if spans.contains(&record) {
                (Some(across), 0)
            } else {
                (widths.get(column).copied(), column)
            };
            let (left, right) = self.padding(padded, columns);
            width.map_or_else(
                || text.to_owned(),
                |width| wrapped(text, width.saturating_sub(left + right), words),
            )
        };
        // The title is a record of its own, spread across every column as
        // the spanning rows are. Why not tabled's `Panel::header`
        // (<https://docs.rs/tabled/0.22.0/tabled/settings/struct.Panel.html#method.header>),
        // which draws the same spread record: it is added after the build,
        // but the title must be in the widths probe and wrapped to the
        // table's width first, as every spanning record is, and share
        // their index list for the alignment, the bold and the rules.
        let mut builder = Builder::default();
        if let Some(title) = &title {
            builder.push_record([fit(title, 0, 0, true)]);
        }
        if head > usize::from(title.is_some()) {
            let heading_record = head - 1;
            builder.push_record(
                headings
                    .iter()
                    .enumerate()
                    .map(|(j, text)| fit(text, heading_record, j, true)),
            );
        }
        for (i, row) in rows.iter().enumerate() {
            let words = |j| !self.anywhere.contains(&j) || self.spanning.contains(&i);
            builder.push_record(
                row.iter()
                    .enumerate()
                    .map(|(j, text)| fit(text, i + head, j, words(j))),
            );
        }
        let mut table = builder.build();
        for &record in &spans {
            table.modify((record, 0), ColumnSpan::spread());
        }
        // Each line of a wrapped cell aligned on its own (tabled aligns a
        // multi-line cell as one block by default), without the spaces
        // wrapping at words leaves at line ends.
        // https://docs.rs/tabled/0.22.0/tabled/settings/formatting/enum.AlignmentStrategy.html
        self.look(&mut table);
        table
            .modify(Segment::all(), AlignmentStrategy::PerLine)
            .modify(Segment::all(), TrimStrategy::Horizontal);
        for &index in &self.right {
            table.modify(Columns::one(index), Alignment::right());
        }
        // After the columns' alignment, so headings (and a title) stay
        // centred, as the spanning rows are; a ruled table's headings
        // keep their columns' alignment.
        if !self.ruled {
            table.modify(Rows::new(..head), Alignment::center());
        }
        for &record in &spans {
            table.modify(Rows::one(record), Alignment::center());
        }
        // The theme's colours, and bold headings only where the theme has
        // colour at all (every role has one, except under `ascii`): `ascii`
        // and colour off write no SGR codes. A ruled table has no border
        // and Muted headings.
        if self.ruled {
            if let Some(muted) = table_colour(theme, Role::Muted, colour) {
                table.modify(Rows::new(..head), muted);
            }
        } else if let Some(border) = table_colour(theme, Role::Border, colour) {
            table.modify(Segment::all(), BorderColor::filled(border));
            table.modify(Rows::new(..head), Color::BOLD);
            for &record in &spans {
                table.modify(Rows::one(record), Color::BOLD);
            }
        }
        for (i, row) in self.rows.iter().enumerate() {
            for (j, cell) in row.iter().enumerate() {
                // An empty cell gets no codes: with none, a ruled line's
                // padding before it is trailing and dropped.
                if cell.text.is_empty() {
                    continue;
                }
                if let Some(paint) = table_style(theme, cell.role, cell.bold, colour) {
                    table.modify((i + head, j), paint);
                }
            }
        }
        // The cells already fit these widths; a column whose cells wrapped
        // at words keeps the width the fit gave it.
        // https://docs.rs/tabled/0.22.0/tabled/settings/width/struct.Width.html#method.list
        table.with(Width::list(widths));
        // The rules above and below a spanning row without the joints of
        // the columns it crosses
        // (<https://docs.rs/tabled/0.22.0/tabled/settings/themes/struct.BorderCorrection.html>).
        if !spans.is_empty() {
            table.with(BorderCorrection::span());
        }
        let text = table.to_string();
        if self.ruled { trimmed(&text) } else { text }
    }

    /// The columns' widths, padding included: their natural widths, capped
    /// at the maximum column width, then fitted to `width` with the widest
    /// column narrowed first, as tabled's `Wrap` chooses them
    /// (<https://docs.rs/tabled/0.22.0/tabled/settings/width/struct.Wrap.html#method.priority>,
    /// `Priority::max`), and how many vertical borders are between them.
    /// `Err` is the line saying so, when `width` is below the narrowest
    /// table.
    fn widths(
        &self,
        title: Option<&str>,
        headings: &[String],
        rows: &[Vec<String>],
        spans: &[usize],
        width: Option<usize>,
    ) -> Result<(Vec<usize>, usize), String> {
        let mut builder = Builder::default();
        if let Some(title) = title {
            builder.push_record([title.to_owned()]);
        }
        if self.heading_rows() > usize::from(title.is_some()) {
            builder.push_record(headings.iter().cloned());
        }
        for row in rows {
            builder.push_record(row.iter().cloned());
        }
        let mut probe = builder.build();
        self.look(&mut probe);
        for &record in spans {
            probe.modify((record, 0), ColumnSpan::spread());
        }
        let columns = probe.count_columns();
        for &(column, min) in &self.min_widths {
            let (left, right) = self.padding(column, columns);
            probe.modify(
                Columns::one(column),
                Width::increase(min.saturating_sub(left + right)),
            );
        }
        if let Some(max) = self.max_column_width {
            probe.modify(Columns::new(..), Width::wrap(max));
        }
        if let Some(width) = width {
            let needed = self.narrowest(columns);
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
        let widths = dimension.get_widths().map_or_default(<[usize]>::to_vec);
        // The vertical borders between the columns, as tabled draws them
        // (papergrid's `has_vertical`; its span width sums the same).
        let columns = widths.len();
        let inner = (1..columns)
            .filter(|&column| probe.get_config().has_vertical(column, columns))
            .count();
        Ok((widths, inner))
    }

    /// The width of the narrowest table of `columns` columns in this
    /// look: a character a cell, measured by tabled. A wide character
    /// (CJK, emoji) needs two; in a one-character column tabled draws it
    /// as `�`.
    fn narrowest(&self, columns: usize) -> usize {
        let mut builder = Builder::default();
        builder.push_record(vec!["x"; columns]);
        let mut table = builder.build();
        self.look(&mut table);
        table.total_width()
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
        let width = crate::stdout_width();
        let colour = crate::colour_on(crate::Stream::Stdout);
        let text = self.render(theme::current(), colour, width);
        // Recorded for the blank line at the end (cli-core's `tail`).
        writeln!(
            sb_cli_core::tail::Tracked::stdout(std::io::stdout()),
            "{text}"
        )
        .map_err(output_error)
    }
}

/// The ruled look's gap between two columns.
const GAP: usize = 2;

/// `text` without the spaces at the end of each line: a ruled line's
/// padding after its last cell (an empty cell's, or a short one's), the
/// codes closed before it. Why not tabled: tabled 0.22 and papergrid 0.18
/// always fill a cell to its column's width, and have no setting to leave
/// a line's last cell unpadded (checked `settings::formatting`,
/// `Padding`, `Width`; `TrimStrategy` trims a cell's own text before it is
/// aligned, not the fill).
fn trimmed(text: &str) -> String {
    let mut lines = text.lines().map(|line| line.trim_end_matches(' '));
    let mut out = lines.next().unwrap_or_default().to_owned();
    for line in lines {
        out.push('\n');
        out.push_str(line);
    }
    out
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

    /// `sb list`'s search table: the first repository as the heading
    /// across both columns, each later one a spanning row, centred.
    fn search() -> Table {
        let mut table = Table::spanning("Saltbox");
        table.row(["plex", "sb install plex"]);
        table.row(["plex-autoscan", "sb install plex-autoscan"]);
        table.spanning_row("Sandbox");
        table.row(["plex-meta-manager", "sb install sandbox-plex-meta-manager"]);
        table
    }

    #[test]
    fn a_spanning_row_crosses_every_column() {
        insta::assert_snapshot!(search().render(&plain(), false, None), @r"
        ╭──────────────────────────────────────────────────────────╮
        │                         Saltbox                          │
        ├───────────────────┬──────────────────────────────────────┤
        │ plex              │ sb install plex                      │
        ├───────────────────┼──────────────────────────────────────┤
        │ plex-autoscan     │ sb install plex-autoscan             │
        ├───────────────────┴──────────────────────────────────────┤
        │                         Sandbox                          │
        ├───────────────────┬──────────────────────────────────────┤
        │ plex-meta-manager │ sb install sandbox-plex-meta-manager │
        ╰───────────────────┴──────────────────────────────────────╯
        ");
    }

    /// A spanning row wider than the columns widens them; fitted, it wraps
    /// across the whole table, not one column.
    #[test]
    fn a_spanning_row_fits_the_table() {
        let mut table = Table::spanning("Saltbox");
        table.row(["a", "b"]);
        table.spanning_row("Saltbox-mod, a long section name");
        insta::assert_snapshot!(table.render(&plain(), false, None), @r"
        ╭──────────────────────────────────╮
        │             Saltbox              │
        ├─────────────────┬────────────────┤
        │ a               │ b              │
        ├─────────────────┴────────────────┤
        │ Saltbox-mod, a long section name │
        ╰──────────────────────────────────╯
        ");
        insta::assert_snapshot!(table.render(&plain(), false, Some(20)), @r"
        ╭────────────────╮
        │    Saltbox     │
        ├────────┬───────┤
        │ a      │ b     │
        ├────────┴───────┤
        │ Saltbox-mod, a │
        │  long section  │
        │      name      │
        ╰────────────────╯
        ");
    }

    /// A `Table::spanning` heading wider than the first column wraps across
    /// the whole table when fitted, not inside that column.
    #[test]
    fn a_spanning_heading_wraps_across_the_table() {
        let mut table = Table::spanning("Saltbox, the first section");
        table.row(["a", "b"]);
        insta::assert_snapshot!(table.render(&plain(), false, Some(20)), @r"
        ╭───────────────╮
        │ Saltbox, the  │
        │ first section │
        ├───────┬───────┤
        │ a     │ b     │
        ╰───────┴───────╯
        ");
    }

    /// Only a `Table::spanning` heading spans: one heading of `Table::new`
    /// over two columns stays in the first.
    #[test]
    fn only_a_spanning_heading_spans() {
        let mut table = Table::new(["Name"]);
        table.row(["a", "b"]);
        insta::assert_snapshot!(table.render(&plain(), false, None), @r"
        ╭──────┬───╮
        │ Name │   │
        ├──────┼───┤
        │ a    │ b │
        ╰──────┴───╯
        ");
    }

    /// With colour, the spanning row is bold as the heading is; without,
    /// no codes.
    #[test]
    fn a_spanning_row_is_bold_with_colour() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let text = search().render(&theme, true, None);
        assert!(text.contains("\x1b[1mSandbox"), "{text:?}");
        assert!(text.contains("\x1b[1mSaltbox"), "{text:?}");
        assert!(!search().render(&theme, false, None).contains('\x1b'));
    }

    /// bench's Speedtest table: a title across every column, then the
    /// column headings, both centred, then the rows with their own
    /// alignment.
    fn speedtest() -> Table {
        let mut table = Table::titled("Speedtest", ["Node", "Upload", "Download", "Latency"])
            .right_align(1)
            .right_align(2)
            .right_align(3);
        table.row(["Speedtest.net", "90.00 Mbps", "940.00 Mbps", "1.50 ms"]);
        table.row(["Paris, FR", "Test failed", "", ""]);
        table
    }

    #[test]
    fn a_title_row_spans_over_the_headings() {
        insta::assert_snapshot!(speedtest().render(&plain(), false, None), @r"
        ╭─────────────────────────────────────────────────────╮
        │                      Speedtest                      │
        ├───────────────┬─────────────┬─────────────┬─────────┤
        │     Node      │   Upload    │  Download   │ Latency │
        ├───────────────┼─────────────┼─────────────┼─────────┤
        │ Speedtest.net │  90.00 Mbps │ 940.00 Mbps │ 1.50 ms │
        ├───────────────┼─────────────┼─────────────┼─────────┤
        │ Paris, FR     │ Test failed │             │         │
        ╰───────────────┴─────────────┴─────────────┴─────────╯
        ");
    }

    /// A title wider than the columns widens them; fitted, it wraps across
    /// the whole table, and a spanning row after it still spans.
    #[test]
    fn a_title_row_fits_the_table() {
        let mut table = Table::titled("A title longer than the columns", ["a", "b"]);
        table.row(["x", "y"]);
        table.spanning_row("Later");
        table.row(["z", "w"]);
        insta::assert_snapshot!(table.render(&plain(), false, None), @r"
        ╭─────────────────────────────────╮
        │ A title longer than the columns │
        ├────────────────┬────────────────┤
        │       a        │       b        │
        ├────────────────┼────────────────┤
        │ x              │ y              │
        ├────────────────┴────────────────┤
        │              Later              │
        ├────────────────┬────────────────┤
        │ z              │ w              │
        ╰────────────────┴────────────────╯
        ");
        insta::assert_snapshot!(table.render(&plain(), false, Some(16)), @r"
        ╭─────────────╮
        │   A title   │
        │ longer than │
        │ the columns │
        ├──────┬──────┤
        │  a   │  b   │
        ├──────┼──────┤
        │ x    │ y    │
        ├──────┴──────┤
        │    Later    │
        ├──────┬──────┤
        │ z    │ w    │
        ╰──────┴──────╯
        ");
    }

    /// With colour, the title and the headings are bold; a cell's role
    /// still paints its cell; without colour, no codes.
    #[test]
    fn a_title_row_is_bold_with_colour() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let mut table = speedtest();
        table.row([
            Cell::new("Tokyo, JP"),
            Cell::new("Test failed").role(Role::Problem),
        ]);
        let text = table.render(&theme, true, None);
        assert!(text.contains("\x1b[1mSpeedtest"), "{text:?}");
        assert!(text.contains("\x1b[1mNode"), "{text:?}");
        assert!(text.contains("\x1b[1mLatency"), "{text:?}");
        assert!(text.contains(&crate::style::paint(
            &theme,
            Role::Problem,
            true,
            "Test failed"
        )));
        assert!(!text.contains("\x1b[1mSpeedtest.net"), "{text:?}");
        assert!(!speedtest().render(&theme, false, None).contains('\x1b'));
    }

    /// The title is sanitized as every heading and cell is.
    #[test]
    fn a_title_is_sanitized() {
        let table = Table::titled("a\x1b[31mb\x07", ["c"]);
        let text = table.render(&plain(), false, None);
        assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text:?}");
        assert!(text.contains("ab"), "{text:?}");
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

    /// A ruled table of bench's Speedtest shape: a 21-wide label column,
    /// numbers right-aligned in 13, 13 and 11 wide columns, a failed row's
    /// text in the second column.
    fn nodes() -> Table {
        let mut table = Table::ruled(["Node", "Upload", "Download", "Latency"])
            .min_width(0, 21)
            .right_align(1)
            .min_width(1, 13)
            .right_align(2)
            .min_width(2, 13)
            .right_align(3)
            .min_width(3, 11);
        table.row(["Speedtest.net", "900.00 Mbps", "950.00 Mbps", "1.00 ms"]);
        table.row([
            Cell::new("Los Angeles, US"),
            Cell::new("Test failed").role(Role::Problem),
        ]);
        table.row(["Tokyo, JP", "600.00 Mbps", "640.00 Mbps", "270.00 ms"]);
        table
    }

    /// The ruled look: no border, no rule between rows, headings aligned as
    /// their columns, the minimum widths counting the two-space gap, and
    /// no line ending in a space.
    #[test]
    fn a_ruled_table_has_no_border_and_fixed_columns() {
        let expected = "\
Node                        Upload     Download    Latency
Speedtest.net          900.00 Mbps  950.00 Mbps    1.00 ms
Los Angeles, US        Test failed
Tokyo, JP              600.00 Mbps  640.00 Mbps  270.00 ms";
        assert_eq!(nodes().render(&plain(), false, None), expected);
        assert_eq!(nodes().render(&plain(), false, Some(80)), expected);
        // A value past its column's minimum widens the column, the gap
        // kept.
        let mut wide = nodes();
        wide.row(["Far", "10000.00 Mbps", "1.00 Mbps", "1.00 ms"]);
        assert_eq!(
            wide.render(&plain(), false, None),
            "\
Node                          Upload     Download    Latency
Speedtest.net            900.00 Mbps  950.00 Mbps    1.00 ms
Los Angeles, US          Test failed
Tokyo, JP                600.00 Mbps  640.00 Mbps  270.00 ms
Far                    10000.00 Mbps    1.00 Mbps    1.00 ms"
        );
    }

    /// No headings, no heading row: label and value rows only, the value
    /// starting after the label column and wrapping at words within the
    /// rest of the width.
    #[test]
    fn a_ruled_table_without_headings_is_its_rows() {
        let mut table = Table::ruled(Vec::<String>::new()).min_width(0, 21);
        table.row(["CPU Cores", "16 @ 3000.000 MHz"]);
        table.row([
            "Skipped",
            "Speedtest unavailable: download release asset \
             \"ookla-speedtest-1.2.0-linux-x86_64.tgz\": error sending request",
        ]);
        assert_eq!(
            table.render(&plain(), false, Some(70)),
            "\
CPU Cores            16 @ 3000.000 MHz
Skipped              Speedtest unavailable: download release asset
                     \"ookla-speedtest-1.2.0-linux-x86_64.tgz\": error
                     sending request"
        );
        // A right-aligned first column has its gap after it.
        let mut first = Table::ruled(Vec::<String>::new()).right_align(0);
        first.row(["1", "x"]);
        first.row(["10", "y"]);
        assert_eq!(first.render(&plain(), false, None), " 1  x\n10  y");
        // A left-aligned column after a right-aligned one keeps a gap (and
        // a right-aligned one after a left-aligned one has both).
        let mut mixed = Table::ruled(["Name", "Num", "Note"]).right_align(1);
        mixed.row(["a", "10", "x"]);
        assert_eq!(
            mixed.render(&plain(), false, None),
            "Name    Num  Note\na        10  x"
        );
        // Too narrow for a character a cell and the gap: said so.
        let mut two = Table::ruled(["a", "b"]);
        two.row(["x", "y"]);
        assert_eq!(two.render(&plain(), false, Some(4)), "a  b\nx  y");
        assert!(
            two.render(&plain(), false, Some(3))
                .starts_with("Too narrow for the table: it needs 4 columns"),
        );
    }

    /// With colour: headings Muted, a cell in its role, a bold cell bold
    /// (in its role, or alone), the padding outside the codes; colour off
    /// or `ascii` writes none, bold included.
    #[test]
    fn a_ruled_table_paints_its_headings_and_cells() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let mut table = nodes();
        table.row([Cell::new("Average").bold(), Cell::new("820.0 MB/s").bold()]);
        table.row([Cell::new("Label").role(Role::Muted).bold(), Cell::new("x")]);
        // An empty cell with a role: no codes, so no padding left before
        // them.
        table.row([Cell::new("Empty"), Cell::new("").role(Role::Problem)]);
        let text = table.render(&theme, true, None);
        let muted = theme.style(Role::Muted);
        let problem = theme.style(Role::Problem);
        assert!(
            text.starts_with(&format!(
                "{muted}Node{muted:#}{}{muted}Upload{muted:#}",
                " ".repeat(24)
            )),
            "{text:?}"
        );
        assert!(
            text.contains(&format!(
                "Los Angeles, US        {problem}Test failed{problem:#}\n"
            )),
            "{text:?}"
        );
        assert!(
            text.contains(&format!(
                "\n\x1b[1mAverage\x1b[0m{}\x1b[1m820.0 MB/s\x1b[0m\n",
                " ".repeat(17)
            )),
            "{text:?}"
        );
        let bold_muted = muted.bold();
        assert!(
            text.contains(&format!(
                "\n{bold_muted}Label{bold_muted:#}{}x\n",
                " ".repeat(28)
            )),
            "{text:?}"
        );
        assert!(text.ends_with("\nEmpty"), "{text:?}");
        assert!(text.lines().all(|line| !line.ends_with(' ')), "{text:?}");
        for (theme, colour) in [(theme, false), (plain(), true)] {
            assert!(!table.render(&theme, colour, None).contains('\x1b'));
        }
    }
}
