//! Announcements in their inline form (tui spec, Scope: "an inline form
//! (tui-markdown's output on stdout, each announcement under its heading,
//! the message through `DisplayText`)"; ops spec, `update`, "Shown"): what
//! `sb update` and `sb announcements` print where the announcement viewer
//! doesn't draw.
//!
//! - The message is sanitized by term-ui's `DisplayText` with colour off
//!   (no escape sequence or control character of the file's reaches the
//!   output), then turned into ratatui `Text` by tui-markdown
//!   (<https://docs.rs/tui-markdown/0.3.12/tui_markdown/fn.from_str_with_options.html>),
//!   whose every colour comes from the [`StyleSheet`] it is given
//!   (<https://docs.rs/tui-markdown/0.3.12/tui_markdown/trait.StyleSheet.html>):
//!   here the run's palette roles, as proto-markdown's reviewed sheet.
//!   Fenced code in a language syntect knows is highlighted by
//!   tui-markdown's `highlight-code` feature (CONSTRAINTS.md E1), in RGB,
//!   brought down to the run's profile as log spans are (ADR-002).
//! - Every span's text goes through `DisplayText` again before it is
//!   written: Markdown's character references (`&#27;`) decode to any code
//!   point but 0 (pulldown-cmark 0.13.6 `scanners.rs`, `scan_entity`), in
//!   text, link destinations and table cells, so the first pass alone
//!   can't keep escapes out. Why not escape `&` before parsing: it would
//!   break `&amp;` and every named reference a message means.
//! - With colour, each span is written in its style as anstyle renders it
//!   (`stream::ansi`, as the log stream's), heading markers and code fences
//!   hidden; without colour (the plain theme: stdout not a terminal, or
//!   `--color=never`, or the `ascii` profile), the text alone, with
//!   tui-markdown's own `#` markers and fences kept, so headings and code
//!   still read as such.
//! - Lines aren't wrapped: piped output keeps each paragraph on one line,
//!   and a terminal wraps what it shows. Only tables, which tui-markdown
//!   lays out itself, take the width when there is one
//!   (<https://docs.rs/tui-markdown/0.3.12/tui_markdown/struct.Options.html#method.table_width>).

use std::borrow::Cow;
use std::io::{self, Write};
use std::sync::LazyLock;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use sb_cli_core::theme::{Profile, Role, Theme};
use sb_term_ui::DisplayText;
use tracing::Dispatch;
use tracing::subscriber::NoSubscriber;
use tui_markdown::{AlertKind, Options, StyleSheet};

use crate::line::style_in;
use crate::stream;

/// One announcement as the inline form shows it: its heading (ops'
/// `<Repo> Announcement - <date>`) and its Markdown message, both as the
/// file gave them (sanitized here).
#[derive(Clone, Copy, Debug)]
pub struct Announcement<'a> {
    pub heading: &'a str,
    pub message: &'a str,
}

/// Write `announcements` on `out` in order, a blank line between two:
/// each heading, a blank line, then its message, in `theme` (the plain
/// theme writes no escape code at all). `width` is the terminal's, when
/// `out` is one: tables are laid out to fit it.
///
/// # Errors
///
/// The write's error.
pub fn write_inline(
    out: &mut impl Write,
    announcements: &[Announcement<'_>],
    theme: &Theme,
    width: Option<u16>,
) -> io::Result<()> {
    for (index, announcement) in announcements.iter().enumerate() {
        if index > 0 {
            out.write_all(b"\n")?;
        }
        write_one(out, announcement, theme, width)?;
    }
    out.flush()
}

fn write_one(
    out: &mut impl Write,
    announcement: &Announcement<'_>,
    theme: &Theme,
    width: Option<u16>,
) -> io::Result<()> {
    // A heading is one line: a date written with a line break in it is
    // joined by a space.
    let heading = one_line(announcement.heading);
    let message = sanitized(announcement.message);
    if theme.profile() == Profile::Ascii {
        writeln!(out, "# {heading}\n")?;
        let text = render_plain(&message, width);
        for line in &text.lines {
            for span in &line.spans {
                out.write_all(safe(&span.content).as_bytes())?;
            }
            out.write_all(b"\n")?;
        }
        return Ok(());
    }
    let sheet = Sheet(theme.clone());
    let heading = Text::from(Line::from(Span::styled(
        format!(" {heading} "),
        sheet.heading(1),
    )));
    write_styled(out, &heading, theme.profile())?;
    out.write_all(b"\n")?;
    let text = render_sheet(&message, sheet, width);
    write_styled(out, &text, theme.profile())
}

/// [`render`] with tui-markdown's own sheet, laid out to `width`. This and
/// [`render_sheet`] aren't generic, so sb-tui alone compiles tui-markdown
/// and pulldown-cmark: [`write_inline`] is generic over its writer, so it
/// is compiled in each caller's crate (sb-ops), and a generic `render`
/// called from it was compiled there again (about 110 KB of the binary;
/// ADR-026).
/// Not inlined, so link-time optimisation can't copy them back.
#[inline(never)]
fn render_plain(message: &str, width: Option<u16>) -> Text<'_> {
    render(message, &with_width(Options::default(), width))
}

/// [`render`] with the palette's sheet, laid out to `width`; not generic,
/// as [`render_plain`].
#[inline(never)]
fn render_sheet(message: &str, sheet: Sheet, width: Option<u16>) -> Text<'_> {
    render(message, &with_width(Options::new(sheet), width))
}

/// tui-markdown's rendering of `message`, with no subscriber: it warns
/// through tracing of a fence language syntect doesn't know
/// (```` ```text ````, tui-markdown 0.3.12 `renderer/code.rs`), which is
/// no trouble of the run's, so the run log doesn't get it. tracing's
/// `dispatcher::with_default` with a `NoSubscriber`
/// (<https://docs.rs/tracing/0.1.44/tracing/dispatcher/fn.with_default.html>,
/// <https://docs.rs/tracing/0.1.44/tracing/subscriber/struct.NoSubscriber.html>),
/// a registered `Dispatch` built once, as the picker's refresh silences
/// its reads (`picker.rs`, why not `Dispatch::none()`).
fn render<'a, S: StyleSheet>(message: &'a str, options: &Options<S>) -> Text<'a> {
    static SILENCE: LazyLock<Dispatch> = LazyLock::new(|| Dispatch::new(NoSubscriber::new()));
    tracing::dispatcher::with_default(&SILENCE, || {
        tui_markdown::from_str_with_options(message, options)
    })
}

/// `message` sanitized by `DisplayText` (colour off), its tabs kept for
/// the Markdown parser. Why not `DisplayText` over the whole message: it
/// expands a tab to the next 8-column stop, and Markdown counts tab stops
/// at 4 (<https://spec.commonmark.org/0.31.2/#tabs>), so a tab-indented
/// sub-list would read as a paragraph. Each line is split
/// at its tabs and each piece sanitized (`DisplayText` keeps no final
/// newline, so lines are split here too, as it splits them); a tab that
/// reaches a rendered span is a control character [`safe`] handles.
fn sanitized(message: &str) -> String {
    let mut clean = String::with_capacity(message.len());
    for (index, line) in message.split('\n').enumerate() {
        if index > 0 {
            clean.push('\n');
        }
        for (index, piece) in line.split('\t').enumerate() {
            if index > 0 {
                clean.push('\t');
            }
            clean.push_str(DisplayText::from_bytes(piece.as_bytes(), false).as_str());
        }
    }
    clean
}

/// What an announcement's Markdown can't show as its author meant
/// (ops decision 35, `sb announcements check`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// HTML, a block or inline: tui-markdown shows it as text. The HTML,
    /// its first line, cut to 40 characters.
    Html(String),
    /// A reference link (`[text][label]`, `[label][]`) whose label the
    /// message doesn't define: shown as its brackets. The label. A
    /// shortcut (`[label]` alone) isn't one: it is prose as often.
    UndefinedLink(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Html(html) => write!(f, "`message` has HTML, which sb shows as text: '{html}'"),
            Self::UndefinedLink(label) => {
                write!(f, "`message` links to '[{label}]', which it doesn't define")
            }
        }
    }
}

/// The [`Problem`]s of `message`, in its order: parsed as tui-markdown
/// parses it (its `from_str_with_options` options, tui-markdown 0.3.12
/// `renderer/mod.rs`), after the same sanitizing, with pulldown-cmark's
/// events for HTML and its broken-link callback for undefined references
/// (<https://docs.rs/pulldown-cmark/0.13.6/pulldown_cmark/struct.Parser.html#method.new_with_broken_link_callback>).
/// HTML inside a code span or block is code, not HTML, and isn't one.
#[must_use]
pub fn problems(message: &str) -> Vec<Problem> {
    use pulldown_cmark::{BrokenLink, Event, LinkType, Options as Parse, Parser, Tag};
    let message = sanitized(message);
    // tui-markdown's options, to re-sync when it is bumped.
    let mut options = Parse::empty();
    for option in [
        Parse::ENABLE_STRIKETHROUGH,
        Parse::ENABLE_TASKLISTS,
        Parse::ENABLE_HEADING_ATTRIBUTES,
        Parse::ENABLE_YAML_STYLE_METADATA_BLOCKS,
        Parse::ENABLE_SUPERSCRIPT,
        Parse::ENABLE_SUBSCRIPT,
        Parse::ENABLE_MATH,
        Parse::ENABLE_FOOTNOTES,
        Parse::ENABLE_DEFINITION_LIST,
        Parse::ENABLE_GFM,
        Parse::ENABLE_TABLES,
    ] {
        options.insert(option);
    }
    let undefined = std::cell::RefCell::new(Vec::new());
    // A shortcut (`[beta]` alone) is prose as often as a link, and
    // tui-markdown shows it as written: only `[text][label]` and
    // `[label][]` name a definition the message lacks.
    let callback = |link: BrokenLink<'_>| {
        if matches!(
            link.link_type,
            LinkType::Shortcut | LinkType::ShortcutUnknown
        ) {
            return None;
        }
        undefined.borrow_mut().push((
            link.span.start,
            Problem::UndefinedLink(link.reference.to_string()),
        ));
        None
    };
    let mut found: Vec<(usize, Problem)> = Vec::new();
    for (event, range) in
        Parser::new_with_broken_link_callback(&message, options, Some(callback)).into_offset_iter()
    {
        match event {
            // A block, one problem: pulldown-cmark wraps its lines' `Html`
            // events in `HtmlBlock`, its range the whole block.
            Event::Start(Tag::HtmlBlock) => {
                let block = message.get(range.clone()).unwrap_or_default();
                found.push((range.start, Problem::Html(snippet(block))));
            }
            Event::InlineHtml(html) => found.push((range.start, Problem::Html(snippet(&html)))),
            _ => {}
        }
    }
    found.extend(undefined.into_inner());
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, problem)| problem).collect()
}

/// `html`'s first line, trimmed, cut to 40 characters (the spec's length,
/// in characters: a problem line's quote, not a column layout; why not
/// console's `truncate_str`, which counts width: sb-tui doesn't depend on
/// console, and the cut needs no width here).
fn snippet(html: &str) -> String {
    let line = html.lines().next().unwrap_or_default().trim();
    let mut cut: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        cut.push('…');
    }
    cut
}

/// `message` as the announcement viewer draws it (tui spec, "The
/// announcement viewer"): sanitized and rendered as the inline form
/// (the palette's sheet; tui-markdown's own sheet, markers and fences
/// kept, with colour off), tables laid out to `width`, each span's text
/// made safe again and its colours brought to the profile, none at all
/// with colour off.
pub(crate) fn screen_text(message: &str, theme: &Theme, width: u16) -> Text<'static> {
    let message = sanitized(message);
    let text = if theme.profile() == Profile::Ascii {
        render(&message, &Options::default().table_width(width))
    } else {
        render(
            &message,
            &Options::new(Sheet(theme.clone())).table_width(width),
        )
    };
    let profile = theme.profile();
    let style = |style| on_screen(style, profile);
    Text {
        lines: text
            .lines
            .into_iter()
            .map(|line| Line {
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span::styled(safe(&span.content).into_owned(), style(span.style)))
                    .collect(),
                style: style(line.style),
                alignment: line.alignment,
            })
            .collect(),
        style: style(text.style),
        alignment: text.alignment,
    }
}

/// `style` for the screen in `profile`: its colours brought down as log
/// spans' are ([`style_in`]); with colour off, none at all, its bold and
/// italic kept. Why not a colourless style sheet instead: syntect's
/// highlighting (`highlight-code`) colours fenced code whatever the sheet
/// says (tui-markdown 0.3.12 `renderer/code.rs`), and `Options` can't turn
/// it off per call; tui-markdown's default sheet, which keeps the
/// markers, has named colours too.
fn on_screen(style: Style, profile: Profile) -> Style {
    if profile == Profile::Ascii {
        Style {
            fg: None,
            bg: None,
            underline_color: None,
            ..style
        }
    } else {
        style_in(style, profile)
    }
}

/// `text` sanitized by `DisplayText` (colour off) on one line, its lines
/// joined by a space.
pub(crate) fn one_line(text: &str) -> String {
    DisplayText::from_bytes(text.as_bytes(), false)
        .lines()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A rendered span's text, as safe to write as the file's own: as it is
/// when it holds no control character (the common case), else through
/// [`one_line`].
fn safe(content: &str) -> Cow<'_, str> {
    if content.chars().any(char::is_control) {
        Cow::Owned(one_line(content))
    } else {
        Cow::Borrowed(content)
    }
}

fn with_width<S: StyleSheet>(options: Options<S>, width: Option<u16>) -> Options<S> {
    match width {
        Some(width) => options.table_width(width),
        None => options,
    }
}

/// `text`'s lines on `out`, each span in its style: the text's, its
/// line's and its own, patched in that order (tui-markdown styles a
/// heading's line, not its spans), with colours in `profile`. Why not
/// ratatui's inline viewport and `Terminal::insert_before`
/// (<https://docs.rs/ratatui/0.30.2/ratatui/struct.Terminal.html#method.insert_before>):
/// it needs a terminal backend and each line's height up front, and lays
/// lines out to the terminal's width, while piped output has no terminal
/// and lines here aren't wrapped.
fn write_styled(out: &mut impl Write, text: &Text<'_>, profile: Profile) -> io::Result<()> {
    for line in &text.lines {
        for span in &line.spans {
            let style = text.style.patch(line.style).patch(span.style);
            let style = stream::ansi(style_in(style, profile));
            write!(
                out,
                "{}{}{}",
                style.render(),
                safe(&span.content),
                style.render_reset()
            )?;
        }
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// The Saltbox look in the run's palette (proto-markdown's `SaltboxSheet`,
/// reviewed 2026-10-29): every element tui-markdown styles takes a role,
/// so nothing falls back to tui-markdown's named colours; heading markers
/// and code fences hidden.
#[derive(Clone, Debug)]
struct Sheet(Theme);

impl Sheet {
    fn fg(&self, role: Role) -> Style {
        Style::from(self.0.style(role))
    }

    fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.0.color(role)))
    }
}

impl StyleSheet for Sheet {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => self
                .fg(Role::OnAccent)
                .patch(self.bg(Role::Accent))
                .add_modifier(Modifier::BOLD),
            2 => self.fg(Role::Accent).add_modifier(Modifier::BOLD),
            3 => self.fg(Role::Text).add_modifier(Modifier::BOLD),
            _ => self
                .fg(Role::Muted)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::ITALIC),
        }
    }

    fn code(&self) -> Style {
        self.fg(Role::Code).patch(self.bg(Role::CodeBg))
    }

    fn link(&self) -> Style {
        self.fg(Role::Link).add_modifier(Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> Style {
        self.fg(Role::Muted).add_modifier(Modifier::ITALIC)
    }

    fn heading_meta(&self) -> Style {
        self.fg(Role::Faint)
    }

    fn metadata_block(&self) -> Style {
        self.fg(Role::Muted)
    }

    fn heading_marker(&self, _level: u8) -> &'static str {
        ""
    }

    fn code_block_fence(&self) -> &'static str {
        ""
    }

    fn html(&self) -> Style {
        self.fg(Role::Muted)
    }

    fn math_inline(&self) -> Style {
        self.code()
    }

    fn math_display(&self) -> Style {
        self.code()
    }

    fn footnote_ref(&self) -> Style {
        self.fg(Role::Link)
    }

    fn footnote_def(&self) -> Style {
        self.fg(Role::Muted)
    }

    fn definition_term(&self) -> Style {
        self.fg(Role::Text).add_modifier(Modifier::BOLD)
    }

    fn definition_description(&self) -> Style {
        self.fg(Role::Text)
    }

    fn alert(&self, kind: AlertKind) -> Style {
        self.fg(match kind {
            AlertKind::Note => Role::Accent,
            AlertKind::Tip => Role::Ok,
            AlertKind::Important => Role::Accent2,
            AlertKind::Warning => Role::Warn,
            AlertKind::Caution => Role::Problem,
        })
    }

    fn table_header(&self) -> Style {
        self.fg(Role::Accent).add_modifier(Modifier::BOLD)
    }

    fn table_cell(&self) -> Style {
        self.fg(Role::Text)
    }

    fn table_border(&self) -> Style {
        self.fg(Role::Border)
    }

    fn list_marker(&self) -> Style {
        self.fg(Role::Accent).add_modifier(Modifier::BOLD)
    }

    fn image_alt(&self) -> Style {
        self.fg(Role::Muted).add_modifier(Modifier::ITALIC)
    }
}

#[cfg(test)]
mod tests {
    use sb_cli_core::theme::Palette;

    use super::*;

    const MESSAGE: &str = "## Feature Update\n\nA **bold** move, with `code`.\n\n\
                           - one\n- two\n\n```yaml\nsettings:\n  enabled: true\n```\n";

    fn written(announcements: &[Announcement<'_>], theme: &Theme, width: Option<u16>) -> String {
        let mut out = Vec::new();
        write_inline(&mut out, announcements, theme, width).expect("written");
        String::from_utf8(out).expect("utf-8")
    }

    fn one(heading: &str, message: &str, theme: &Theme) -> String {
        written(&[Announcement { heading, message }], theme, None)
    }

    fn theme(profile: Profile) -> Theme {
        Theme::resolve(&Palette::default(), profile)
    }

    /// Every `ESC [ ... m` in `text`, each as its parameters.
    fn sgr_codes(text: &str) -> Vec<String> {
        text.split('\x1b')
            .skip(1)
            .filter_map(|rest| rest.strip_prefix('['))
            .filter_map(|rest| rest.split_once('m').map(|(params, _)| params.to_string()))
            .collect()
    }

    #[test]
    fn plain_is_the_heading_then_the_markdown_as_text() {
        let shown = one(
            "Saltbox Announcement - January 15, 2026",
            MESSAGE,
            &Theme::plain(),
        );
        assert_eq!(
            shown,
            "# Saltbox Announcement - January 15, 2026\n\n\
             ## Feature Update\n\n\
             A bold move, with code.\n\n\
             - one\n- two\n\n\
             ```yaml\nsettings:\n  enabled: true\n```\n"
        );
    }

    #[test]
    fn announcements_follow_one_another_after_a_blank_line() {
        let shown = written(
            &[
                Announcement {
                    heading: "first",
                    message: "one",
                },
                Announcement {
                    heading: "second",
                    message: "two",
                },
            ],
            &Theme::plain(),
            None,
        );
        assert_eq!(shown, "# first\n\none\n\n# second\n\ntwo\n");
    }

    #[test]
    fn nothing_of_the_files_escapes_or_controls_reaches_the_output() {
        let message = "\x1b]0;title\x07hi \x1b[31mred\x1b[0m\x07 \u{9b}2Jthere";
        for theme in [Theme::plain(), theme(Profile::TrueColor)] {
            let shown = one("date\x1b[2J\nnext", message, &theme);
            assert!(!shown.contains("title"), "{shown:?}");
            assert!(
                !shown.contains('\x07') && !shown.contains('\u{9b}'),
                "{shown:?}"
            );
            assert!(shown.contains("hi red there"), "{shown:?}");
            assert!(shown.contains("date next"), "{shown:?}");
            // Only the sheet's own colour codes: none of the file's.
            assert!(
                sgr_codes(&shown)
                    .iter()
                    .all(|code| code != "31" && code != "2J"),
                "{shown:?}"
            );
        }
    }

    /// Markdown's character references decode to any code point but 0
    /// (pulldown-cmark), in text, link destinations and table cells: what
    /// they decode to is sanitized as the file's own bytes are.
    #[test]
    fn character_references_bring_no_escape_or_control_back() {
        let message = "a &#27;[2J b &#x1b;]0;t&#7; c &#155;31m d &#x9b;H e &#13;f\n\n\
                       [link](&#27;]52;c;eA==&#7;)\n\n| x |\n|---|\n| &#27;[H&#8;y |\n";
        for theme in [Theme::plain(), theme(Profile::TrueColor)] {
            let shown = one("h", message, &theme);
            let sheet_codes: String = sgr_codes(&shown).concat();
            let without_sgr: String = shown
                .split('\x1b')
                .enumerate()
                .map(|(i, piece)| {
                    if i == 0 {
                        piece
                    } else {
                        // Every ESC left must start one of the sheet's SGRs.
                        let (params, rest) = piece
                            .strip_prefix('[')
                            .and_then(|p| p.split_once('m'))
                            .unwrap_or_else(|| panic!("a stray ESC: {shown:?}"));
                        assert!(
                            params.chars().all(|c| c.is_ascii_digit() || c == ';'),
                            "{shown:?}"
                        );
                        rest
                    }
                })
                .collect();
            for control in ['\x07', '\x08', '\r', '\u{9b}'] {
                assert!(!without_sgr.contains(control), "{control:?} in {shown:?}");
            }
            assert!(!sheet_codes.contains("2J") && !sheet_codes.contains("52"));
            assert!(
                without_sgr.contains("a ") && without_sgr.contains('y'),
                "{without_sgr:?}"
            );
        }
    }

    /// Tabs reach the Markdown parser as tabs: `DisplayText` would expand
    /// them to 8-column stops, and Markdown counts tab stops at 4, so a
    /// tab-indented sub-list became one paragraph (the code review of
    /// M74).
    #[test]
    fn tabs_keep_their_markdown_meaning() {
        let shown = one("h", "- item\n\t- sub\n\n-\ttabbed item\n", &Theme::plain());
        let spaces = one("h", "- item\n    - sub\n\n- tabbed item\n", &Theme::plain());
        assert_eq!(shown, spaces);
        assert!(shown.contains("- item\n    - sub\n"), "{shown:?}");
    }

    /// Lines and tabs come through as they were, every piece between them
    /// sanitized.
    #[test]
    fn sanitizing_keeps_lines_and_tabs() {
        assert_eq!(sanitized("a\n\tb\x07\tc\n"), "a\n\tb\tc\n");
        assert_eq!(sanitized("\x1b[31mx\x1b[0m"), "x");
    }

    #[test]
    fn plain_writes_no_escape_code() {
        let shown = one("heading", MESSAGE, &Theme::plain());
        assert!(!shown.contains('\x1b'), "{shown:?}");
    }

    #[test]
    fn colour_hides_the_markers_and_styles_the_heading() {
        let theme = theme(Profile::TrueColor);
        let shown = one("Saltbox Announcement - January 15, 2026", MESSAGE, &theme);
        assert!(!shown.contains("## "), "{shown:?}");
        assert!(!shown.contains("```"), "{shown:?}");
        let heading = Sheet(theme.clone()).heading(1);
        let style = stream::ansi(heading);
        assert!(
            shown.starts_with(&format!(
                "{}{}{}\n\n",
                style.render(),
                " Saltbox Announcement - January 15, 2026 ",
                style.render_reset()
            )),
            "{shown:?}"
        );
        let plain_text: String = shown
            .split('\x1b')
            .map(|piece| piece.split_once('m').map_or(piece, |(_, rest)| rest))
            .collect();
        // No marker or fence of any kind takes a line or a line's start.
        assert_eq!(
            plain_text,
            " Saltbox Announcement - January 15, 2026 \n\nFeature Update\n\n\
             A bold move, with code.\n\n- one\n- two\n\nsettings:\n  enabled: true\n"
        );
    }

    #[test]
    fn every_colour_fits_the_profile_the_highlighted_code_too() {
        for (profile, rgb, xterm) in [
            (Profile::TrueColor, true, false),
            (Profile::Ansi256, false, true),
            (Profile::Ansi, false, false),
        ] {
            let shown = one("heading", MESSAGE, &theme(profile));
            let codes = sgr_codes(&shown);
            assert!(!codes.is_empty(), "{profile:?}");
            let has = |marker: &str| codes.iter().any(|c| c.contains(marker));
            assert_eq!(has("38;2;") || has("48;2;"), rgb, "{profile:?}: {codes:?}");
            assert_eq!(
                has("38;5;") || has("48;5;"),
                xterm,
                "{profile:?}: {codes:?}"
            );
        }
    }

    #[test]
    fn a_table_fits_the_width_given() {
        let message =
            "| Setting | Meaning |\n|---|---|\n| enabled | a long cell that has to wrap |\n";
        let shown = written(
            &[Announcement {
                heading: "h",
                message,
            }],
            &Theme::plain(),
            Some(24),
        );
        let width = |line: &str| Line::raw(line.to_string()).width();
        let widest = shown.lines().map(width).max().unwrap_or_default();
        assert!(widest <= 24, "{shown}");
        let natural = one("h", message, &Theme::plain());
        assert!(natural.lines().any(|line| width(line) > 24), "{natural}");
    }

    #[test]
    fn every_role_of_the_sheet_is_the_palettes() {
        let theme = theme(Profile::TrueColor);
        let sheet = Sheet(theme.clone());
        let fg = |role| Style::from(theme.style(role)).fg;
        assert_eq!(sheet.heading(1).fg, fg(Role::OnAccent));
        assert_eq!(
            sheet.heading(1).bg,
            theme.color(Role::Accent).map(Into::into)
        );
        assert_eq!(sheet.heading(2).fg, fg(Role::Accent));
        assert_eq!(sheet.heading(3).fg, fg(Role::Text));
        assert_eq!(sheet.heading(4).fg, fg(Role::Muted));
        assert_eq!(sheet.code().fg, fg(Role::Code));
        assert_eq!(sheet.code().bg, theme.color(Role::CodeBg).map(Into::into));
        assert_eq!(sheet.link().fg, fg(Role::Link));
        assert_eq!(sheet.blockquote().fg, fg(Role::Muted));
        assert_eq!(sheet.heading_meta().fg, fg(Role::Faint));
        assert_eq!(sheet.metadata_block().fg, fg(Role::Muted));
        assert_eq!(sheet.html().fg, fg(Role::Muted));
        assert_eq!(sheet.math_inline(), sheet.code());
        assert_eq!(sheet.math_display(), sheet.code());
        assert_eq!(sheet.footnote_ref().fg, fg(Role::Link));
        assert_eq!(sheet.footnote_def().fg, fg(Role::Muted));
        assert_eq!(sheet.definition_term().fg, fg(Role::Text));
        assert_eq!(sheet.definition_description().fg, fg(Role::Text));
        assert_eq!(sheet.alert(AlertKind::Note).fg, fg(Role::Accent));
        assert_eq!(sheet.alert(AlertKind::Tip).fg, fg(Role::Ok));
        assert_eq!(sheet.alert(AlertKind::Important).fg, fg(Role::Accent2));
        assert_eq!(sheet.alert(AlertKind::Warning).fg, fg(Role::Warn));
        assert_eq!(sheet.alert(AlertKind::Caution).fg, fg(Role::Problem));
        assert_eq!(sheet.table_header().fg, fg(Role::Accent));
        assert_eq!(sheet.table_cell().fg, fg(Role::Text));
        assert_eq!(sheet.table_border().fg, fg(Role::Border));
        assert_eq!(sheet.list_marker().fg, fg(Role::Accent));
        assert_eq!(sheet.image_alt().fg, fg(Role::Muted));
        for style in [
            sheet.heading(1),
            sheet.heading(2),
            sheet.heading(3),
            sheet.definition_term(),
            sheet.table_header(),
            sheet.list_marker(),
        ] {
            assert!(style.add_modifier.contains(Modifier::BOLD), "{style:?}");
        }
        for style in [sheet.heading(4), sheet.blockquote(), sheet.image_alt()] {
            assert!(style.add_modifier.contains(Modifier::ITALIC), "{style:?}");
        }
        assert!(sheet.link().add_modifier.contains(Modifier::UNDERLINED));
    }

    /// HTML, a block (one problem for its lines) and inline, and an
    /// undefined reference link are problems, in the message's order;
    /// HTML in code, a defined reference, a plain link and autolinks
    /// aren't.
    #[test]
    fn the_markdown_problems() {
        assert_eq!(
            problems(
                "Text <b>bold</b> and [docs][guide].\n\n<div>\nraw\n</div>\n\nAlso [missing][], not [beta]."
            ),
            vec![
                Problem::Html("<b>".to_owned()),
                Problem::Html("</b>".to_owned()),
                Problem::UndefinedLink("guide".to_owned()),
                Problem::Html("<div>".to_owned()),
                Problem::UndefinedLink("missing".to_owned()),
            ]
        );
        assert_eq!(
            problems(
                "`<b>` in code, [defined][d], [plain](https://example.com), \
                 <https://example.com>.\n\n```html\n<div></div>\n```\n\n[d]: https://example.com\n"
            ),
            Vec::<Problem>::new()
        );
        assert_eq!(
            Problem::Html("<b>".to_owned()).to_string(),
            "`message` has HTML, which sb shows as text: '<b>'"
        );
        assert_eq!(
            Problem::UndefinedLink("guide".to_owned()).to_string(),
            "`message` links to '[guide]', which it doesn't define"
        );
        let long = format!("<p class=\"{}\">", "x".repeat(60));
        let Some(Problem::Html(shown)) = problems(&long).into_iter().next() else {
            panic!("no problem for {long}");
        };
        assert_eq!(shown.chars().count(), 41, "{shown}");
        assert!(shown.ends_with('…'), "{shown}");
    }
}
