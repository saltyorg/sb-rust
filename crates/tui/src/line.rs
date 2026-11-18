//! A log line as screen text (tui spec, "Lines" and "Text path"):
//! `2026-11-13T10:04:05+0100 <origin>: <message>`, local time to the
//! second (journalctl's `short-iso` without the host name), the message
//! cut at the record cap with a Muted notice, sanitized by term-ui's
//! `DisplayText`, then turned into spans by ansi-to-tui.
//!
//! - ansi-to-tui's `IntoText`
//!   (<https://docs.rs/ansi-to-tui/8.0.1/ansi_to_tui/trait.IntoText.html>)
//!   parses the SGR `DisplayText` keeps; nothing else reaches it.
//! - Under `ansi256` and `ansi` a log span's colours go through
//!   anstyle-lossy (`color_to_xterm`, `color_to_ansi`,
//!   <https://docs.rs/anstyle-lossy/1.1.5/anstyle_lossy/>) as the palette's
//!   do (ADR-002 decisions 3 and 5), through ratatui-core's `anstyle`
//!   conversions; under `ascii` the text is built with colour off.
//! - Local time is `/etc/localtime` through jiff's `TimeZone::tzif`
//!   (<https://docs.rs/jiff/0.2.38/jiff/tz/struct.TimeZone.html#method.tzif>;
//!   the tui spec's "Lines", as config decision 7 reads zones), else UTC.
//!   Why not `TimeZone::try_system`: it needs jiff's `tz-system` feature
//!   (and `tzdb-zoneinfo` for a named `TZ`), off workspace-wide; the spec
//!   names `/etc/localtime`, so `TZ` isn't read (journalctl's is).

use ansi_to_tui::IntoText as _;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span, Text};
use sb_cli_core::theme::{Profile, Role, Theme};
use sb_term_ui::DisplayText;

use crate::search;
use crate::source::Line;

/// The record cap: a message's bytes past it aren't shown (tui decision
/// 12).
pub const RECORD_CAP: usize = 64 << 10;

/// The most of an origin shown (and held, [`Line::new`]): a service sets
/// its own identifier, so it is untrusted and unbounded too.
pub const ORIGIN_CAP: usize = 256;

/// The local time zone: `/etc/localtime`'s `TZif` data, else UTC.
#[must_use]
pub fn local_zone() -> TimeZone {
    zone_from(std::fs::read("/etc/localtime").ok().as_deref())
}

/// `data` as a `TZif` zone, or UTC without one or when it doesn't parse.
fn zone_from(data: Option<&[u8]>) -> TimeZone {
    data.and_then(|data| TimeZone::tzif("localtime", data).ok())
        .unwrap_or(TimeZone::UTC)
}

/// How lines are drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    /// Time and origin shown (`t`).
    pub meta: bool,
    pub zone: TimeZone,
    /// The run's theme: its profile decides the log spans' colours, its
    /// Muted role the cut notice.
    pub theme: Theme,
    /// Bytes of a message shown ([`RECORD_CAP`]; smaller in tests).
    pub record_cap: usize,
}

impl Format {
    /// The viewer's default: time and origin shown, the record cap.
    #[must_use]
    pub fn new(zone: TimeZone, theme: Theme) -> Self {
        Self {
            meta: true,
            zone,
            theme,
            record_cap: RECORD_CAP,
        }
    }

    pub(crate) fn style(&self, role: Role) -> Style {
        Style::from(self.theme.style(role))
    }

    /// `role`'s colour as a background, none under `ascii`.
    pub(crate) fn bg(&self, role: Role) -> Style {
        Style::from(anstyle::Style::new().bg_color(self.theme.color(role)))
    }

    /// How a search match is lit (the maintainer, 2026-11-14; tui
    /// decision 23): the `SearchHit` background under truecolor and
    /// ansi256, reverse video under ansi and ascii. The 16-colour pick,
    /// yellow, hid a yellow span inside a match, and ascii has no colour;
    /// ratatui's `Modifier::REVERSED`
    /// (<https://docs.rs/ratatui/0.30.2/ratatui/style/struct.Modifier.html>)
    /// is patched over each span's own colours.
    pub(crate) fn hit(&self) -> Style {
        match self.theme.profile() {
            Profile::TrueColor | Profile::Ansi256 => self.bg(Role::SearchHit),
            Profile::Ansi | Profile::Ascii => Style::new().add_modifier(Modifier::REVERSED),
        }
    }

    pub(crate) fn colour(&self) -> bool {
        self.theme.profile() != Profile::Ascii
    }
}

/// `line` as screen text: one screen line per line of the message, the
/// time and origin before the first, the cut notice after the last.
#[must_use]
pub fn text(line: &Line, format: &Format) -> Text<'static> {
    searched(line, format, "", Style::new())
}

/// The message's bytes shown: cut at the record cap, and how many went,
/// those its source didn't keep ([`Line::cut`]) too.
pub(crate) fn shown<'a>(line: &'a Line, format: &Format) -> (&'a [u8], usize) {
    // A byte cut: it can split a character (one U+FFFD before the notice)
    // or an SGR (dropped by `DisplayText`), both harmless.
    let cut = line.message.len().saturating_sub(format.record_cap);
    (&line.message[..line.message.len() - cut], cut + line.cut)
}

/// What search reads of `line` (tui spec, "Search"): the message's screen
/// lines as the screen shows them with colour off (`DisplayText`, research
/// decision 10), cut at the record cap. The time and origin aren't
/// searched, as journalctl's `--grep` matches the message alone
/// (<https://www.freedesktop.org/software/systemd/man/latest/journalctl.html#-g>).
fn searchable(line: &Line, format: &Format) -> DisplayText {
    DisplayText::from_bytes(shown(line, format).0, false)
}

/// The matches of `query` in `line`'s message ([`search::matches`]).
pub(crate) fn hits(line: &Line, format: &Format, query: &str) -> usize {
    searchable(line, format)
        .lines()
        .map(|l| search::matches(l, query).len())
        .sum()
}

/// Where the `nth` match of `query` in `line` starts on screen: the
/// screen line of the entry it is on, the text drawn before it on that
/// line (the time and origin too, on the first), and the rest of the word
/// it starts in (a word wrap moves a word whole).
pub(crate) fn hit_lead(
    line: &Line,
    format: &Format,
    query: &str,
    nth: usize,
) -> Option<(usize, String, String)> {
    let plain = searchable(line, format);
    let (row, text, at) = plain
        .lines()
        .enumerate()
        .flat_map(|(row, text)| {
            search::matches(text, query)
                .into_iter()
                .map(move |m| (row, text, m.start))
        })
        .nth(nth)?;
    let mut lead = if row == 0 && format.meta {
        meta(line, format)
    } else {
        String::new()
    };
    lead.push_str(&text[..at]);
    let word = text[at..]
        .split(char::is_whitespace)
        .next()
        .unwrap_or_default()
        .to_owned();
    Some((row, lead, word))
}

/// [`text`] with every match of `query` in the message patched with `hit`
/// ([`Format::hit`]); the time, origin and cut notice aren't searched.
pub(crate) fn searched(line: &Line, format: &Format, query: &str, hit: Style) -> Text<'static> {
    let (shown, cut) = shown(line, format);
    build(line, format, shown, cut, query, hit)
}

/// [`searched`] as a window with wrap off shows it: its first `rows`
/// screen lines, each right through column `columns` (the time and
/// origin counted), and maybe some of what follows; with whether that is
/// the whole message's text, so that no window needs it built again.
/// Only as much of the
/// message as that needs is built, the work of a frame with wrap off
/// (M73's follow-up: building every record in view whole, `DisplayText`
/// over 5,000+ bytes each, cost a frame of 70 of them 5 to 10 ms at
/// 240×70).
///
/// The text of a prefix of the message is that of the whole message up
/// to the prefix's end: `DisplayText` and ansi-to-tui read in order,
/// and the end can only split a character (one U+FFFD) or drop an
/// unfinished escape sequence. The exceptions are handled: a carriage
/// return with text after it, which takes back what came before it on
/// its line, on the line the prefix ends on or after it, means the
/// whole message is built ([`takes_back`]); a match that starts in the
/// window must be in the prefix whole, so the prefix reaches past
/// `columns` by twice the query's characters (at most two columns
/// each), and by two more for a character the end splits. The cut
/// notice is past the window and left out. The prefix starts at twice
/// the columns in bytes and doubles until it fills the window.
/// UNVERIFIED for ansi-to-tui, whose docs say nothing of a prefix: checked
/// by drawing both texts in `a_window_draws_within_as_the_whole_text`
/// (colour across the end, tabs, wide, combining and split characters,
/// carriage returns, searches).
pub(crate) fn within(
    line: &Line,
    format: &Format,
    query: &str,
    hit: Style,
    rows: usize,
    columns: usize,
) -> (Text<'static>, bool) {
    let (shown, cut) = shown(line, format);
    let rows = rows.max(1);
    let columns = columns + 2 * query.to_lowercase().chars().count() + 2;
    let mut end = columns.saturating_mul(2);
    loop {
        if end >= shown.len() || takes_back(shown, end) {
            return (build(line, format, shown, cut, query, hit), true);
        }
        let text = build(line, format, &shown[..end], 0, query, hit);
        let filled = match text.lines.len().cmp(&rows) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => text.lines[rows - 1].width() >= columns,
            std::cmp::Ordering::Less => false,
        };
        if filled {
            return (text, false);
        }
        end = end.saturating_mul(2);
    }
}

/// Whether a carriage return on the line the prefix `..end` of `bytes`
/// ends on, or after it, is followed by something other than another
/// carriage return or a newline: `DisplayText` may then drop what came
/// before it on its line, which the prefix's text still shows. The scan
/// starts at that line's start, not at the prefix's end: a carriage
/// return stays pending across bytes that print nothing (a C1 code,
/// the rest of an escape sequence), so what takes its line back can
/// come after the end though the return came before it.
pub(crate) fn takes_back(bytes: &[u8], end: usize) -> bool {
    let from = bytes[..end]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |at| at + 1);
    bytes[from..]
        .windows(2)
        .any(|pair| pair[0] == b'\r' && !matches!(pair[1], b'\r' | b'\n'))
}

/// How many screen lines `line` takes with wrap off: [`text`]'s lines,
/// counted without building it. A newline ends a screen line, a final
/// one adds none, and an empty message is one line (`DisplayText`), so
/// the count is the newlines', from the bytes, unless a newline can be
/// inside a string sequence (OSC, DCS, SOS, PM, APC: `ESC` and `]`, `P`,
/// `X`, `^` or `_`), whose payload the DEC parser swallows newlines and
/// all, or the message holds a C1 code (UTF-8 `C2 80` to `C2 9F`), which
/// starts one or which `DisplayText` drops; then `DisplayText` counts
/// them. Everywhere else the parser executes a newline, in an escape or
/// control sequence too (<https://vt100.net/emu/dec_ansi_parser>, the
/// `execute` action of every state but the string ones; anstyle-parse
/// 1.0.0's table). Why not anstyle-parse's `Parser` with a `Perform`
/// that counts `execute(b'\n')`
/// (<https://docs.rs/anstyle-parse/1.0.0/anstyle_parse/trait.Perform.html>),
/// which follows the table itself: on a 5,000-byte record it took 17 µs
/// against the scan's 5 µs (release, by hand), about 1.2 ms against
/// 0.35 ms for the 70 records a switch to wrap off measures, and it
/// would make anstyle-parse a direct dependency of this crate; the
/// tests pin the scan to `DisplayText`'s count either way.
pub(crate) fn screen_lines(line: &Line, format: &Format) -> usize {
    let shown = shown(line, format).0;
    let hidden = shown.windows(2).any(|pair| {
        matches!(
            pair,
            [0x1b, b']' | b'P' | b'X' | b'^' | b'_'] | [0xc2, 0x80..=0x9f]
        )
    });
    if hidden {
        return searchable(line, format).lines().len();
    }
    shown.split(|&b| b == b'\n').count() - usize::from(shown.last() == Some(&b'\n'))
}

/// [`searched`] of `shown`, the message's bytes to show, with the cut
/// notice for `cut` bytes not shown.
fn build(
    line: &Line,
    format: &Format,
    shown: &[u8],
    cut: usize,
    query: &str,
    hit: Style,
) -> Text<'static> {
    let colour = format.colour();
    let display = DisplayText::from_bytes(shown, colour);
    let mut text = if colour {
        // `DisplayText` keeps only SGR, which ansi-to-tui parses; should it
        // still refuse the text, it is shown without colour.
        display.as_str().into_text().map_or_else(
            |_| plain(&DisplayText::from_bytes(shown, false)),
            |text| lossy(text, format.theme.profile()),
        )
    } else {
        plain(&display)
    };
    // ansi-to-tui drops a trailing empty line; `DisplayText`'s lines are
    // the screen's.
    while text.lines.len() < display.lines().len() {
        text.lines.push(TextLine::default());
    }
    // A caller's empty style (as `text` passes): nothing to light.
    if !query.is_empty() && hit != Style::new() {
        for screen_line in &mut text.lines {
            search::highlight(screen_line, query, hit);
        }
    }
    if format.meta {
        let prefix = meta(line, format);
        if !prefix.is_empty() {
            text.lines[0].spans.insert(0, Span::raw(prefix));
        }
    }
    if cut > 0 {
        let notice = Span::styled(
            format!(" … [{cut} bytes not shown]"),
            format.style(Role::Muted),
        );
        if let Some(last) = text.lines.last_mut() {
            last.spans.push(notice);
        }
    }
    text
}

/// `display`'s lines as unstyled text, line by line as `DisplayText`
/// splits them (why not `Text::raw`: it splits with `str::lines`, which
/// drops a trailing empty line `DisplayText` keeps).
fn plain(display: &DisplayText) -> Text<'static> {
    Text::from(
        display
            .lines()
            .map(|l| TextLine::raw(l.to_owned()))
            .collect::<Vec<_>>(),
    )
}

/// `time origin: `, the parts that are known.
pub(crate) fn meta(line: &Line, format: &Format) -> String {
    let mut prefix = String::new();
    if let Some(time) = line.time {
        prefix.push_str(&local(time, &format.zone));
        prefix.push(' ');
    }
    if !line.origin.is_empty() {
        let cut = &line.origin[..line.origin.len().min(ORIGIN_CAP)];
        let origin = DisplayText::from_bytes(cut, false);
        // One line: an origin with a newline shows its first.
        prefix.push_str(origin.lines().next().unwrap_or_default());
        prefix.push_str(": ");
    }
    prefix
}

/// `time` in `zone` to the second, with its offset: `short-iso`.
pub(crate) fn local(time: Timestamp, zone: &TimeZone) -> String {
    time.to_zoned(zone.clone())
        .strftime("%Y-%m-%dT%H:%M:%S%z")
        .to_string()
}

/// `text`'s colours in `profile`: RGB and 256 colours brought down by
/// anstyle-lossy, as the palette is.
fn lossy(mut text: Text<'static>, profile: Profile) -> Text<'static> {
    for line in &mut text.lines {
        lossy_line(line, profile);
    }
    text
}

/// [`lossy`] for one line.
pub(crate) fn lossy_line(line: &mut TextLine<'_>, profile: Profile) {
    for span in &mut line.spans {
        span.style = style_in(span.style, profile);
    }
}

/// `style`'s colours in `profile` ([`colour_in`]): the log spans' and the
/// announcements' one conversion.
pub(crate) fn style_in(style: Style, profile: Profile) -> Style {
    Style {
        fg: style.fg.map(|c| colour_in(c, profile)),
        bg: style.bg.map(|c| colour_in(c, profile)),
        underline_color: style.underline_color.map(|c| colour_in(c, profile)),
        ..style
    }
}

/// `colour` in `profile`: anstyle-lossy passes what already fits. Reset
/// and the 16 named colours pass as they are, before the conversion:
/// ratatui-core's `From<Color> for anstyle::Color` panics on `Reset`.
fn colour_in(colour: Color, profile: Profile) -> Color {
    if !matches!(colour, Color::Rgb(..) | Color::Indexed(_)) {
        return colour;
    }
    let anstyle = anstyle::Color::from(colour);
    match profile {
        Profile::TrueColor | Profile::Ascii => colour,
        Profile::Ansi256 => Color::from(anstyle_lossy::color_to_xterm(anstyle)),
        Profile::Ansi => Color::from(anstyle_lossy::color_to_ansi(
            anstyle,
            anstyle_lossy::palette::VGA,
        )),
    }
}

#[cfg(test)]
mod tests {
    use sb_cli_core::theme::Palette;

    use super::*;
    use crate::testing;

    fn format(profile: Profile) -> Format {
        Format::new(TimeZone::UTC, Theme::resolve(&Palette::default(), profile))
    }

    fn service_line(message: &[u8]) -> Line {
        Line {
            time: Some("2026-11-13T09:04:05Z".parse().expect("a time")),
            origin: b"app[7]".to_vec(),
            message: message.to_vec(),
            cut: 0,
            position: testing::position(1),
        }
    }

    fn strings(text: &Text<'_>) -> Vec<String> {
        text.lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn a_service_line_has_its_time_and_origin() {
        let text = text(&service_line(b"started"), &format(Profile::Ascii));
        assert_eq!(strings(&text), ["2026-11-13T09:04:05+0000 app[7]: started"]);
    }

    #[test]
    fn the_time_is_local_with_its_offset() {
        let mut f = format(Profile::Ascii);
        f.zone = jiff::tz::TimeZone::fixed(jiff::tz::offset(1));
        let text = text(&service_line(b"x"), &f);
        assert_eq!(strings(&text), ["2026-11-13T10:04:05+0100 app[7]: x"]);
    }

    #[test]
    fn no_zone_file_is_utc() {
        assert_eq!(zone_from(None), TimeZone::UTC);
        assert_eq!(zone_from(Some(b"not tzif")), TimeZone::UTC);
        // The host's own zone, whatever it is, gives a whole-minute offset.
        let offset = local_zone().to_offset(Timestamp::UNIX_EPOCH);
        assert_eq!(offset.seconds() % 60, 0);
    }

    #[test]
    fn t_hides_the_time_and_origin() {
        let mut f = format(Profile::Ascii);
        f.meta = false;
        assert_eq!(strings(&text(&service_line(b"started"), &f)), ["started"]);
    }

    #[test]
    fn a_container_line_shows_its_stream_and_an_unknown_part_is_left_out() {
        let f = format(Profile::Ascii);
        let mut line = service_line(b"hello");
        line.origin = b"stderr".to_vec();
        line.time = None;
        assert_eq!(strings(&text(&line, &f)), ["stderr: hello"]);
        line.origin.clear();
        assert_eq!(strings(&text(&line, &f)), ["hello"]);
    }

    #[test]
    fn a_message_with_newlines_is_several_screen_lines_of_one_entry() {
        let text = text(&service_line(b"one\ntwo\n"), &format(Profile::Ascii));
        assert_eq!(
            strings(&text),
            ["2026-11-13T09:04:05+0000 app[7]: one", "two"]
        );
        // Colour on or off, the same screen lines, a trailing empty one
        // included.
        for profile in [Profile::Ascii, Profile::TrueColor] {
            let text = super::text(&service_line(b"a\n\nb\n\n"), &format(profile));
            assert_eq!(
                strings(&text),
                ["2026-11-13T09:04:05+0000 app[7]: a", "", "b", ""],
                "{profile:?}"
            );
        }
        let empty = super::text(&service_line(b""), &format(Profile::Ascii));
        assert_eq!(strings(&empty), ["2026-11-13T09:04:05+0000 app[7]: "]);
    }

    #[test]
    fn a_record_over_the_cap_is_cut_with_its_notice() {
        let mut f = format(Profile::Ascii);
        f.record_cap = 4;
        let text = text(&service_line(b"abcdefghij"), &f);
        assert_eq!(
            strings(&text),
            ["2026-11-13T09:04:05+0000 app[7]: abcd … [6 bytes not shown]"]
        );
        let at_cap = super::text(&service_line(b"abcd"), &f);
        assert_eq!(strings(&at_cap), ["2026-11-13T09:04:05+0000 app[7]: abcd"]);
    }

    #[test]
    fn the_default_cap_is_64_kib() {
        let f = format(Profile::Ascii);
        assert_eq!(f.record_cap, 65_536);
        let big = vec![b'x'; 1 << 20];
        let text = text(&service_line(&big), &f);
        let last = text.lines[0].spans.last().expect("the notice");
        assert_eq!(last.content, " … [983040 bytes not shown]");
    }

    /// A line its source cut shows the whole count of what went, the
    /// source's cut and the viewer's own.
    #[test]
    fn a_line_cut_by_its_source_counts_every_byte_not_shown() {
        let big = vec![b'x'; 1 << 20];
        let mut line = Line::new(None, b"", &big, testing::position(1));
        let f = format(Profile::Ascii);
        let text = text(&line, &f);
        let last = text.lines[0].spans.last().expect("the notice");
        assert_eq!(last.content, " … [983040 bytes not shown]");
        let mut small = format(Profile::Ascii);
        small.record_cap = 4;
        line.message.truncate(10);
        let text = super::text(&line, &small);
        let last = text.lines[0].spans.last().expect("the notice");
        assert_eq!(
            last.content,
            format!(" … [{} bytes not shown]", 983_040 + 6)
        );
    }

    #[test]
    fn tabs_expand_and_escapes_other_than_colour_go() {
        let line = service_line(b"a\tb\x1b]0;title\x07\x1b[2J\xc2\x9bc");
        let text = text(&line, &format(Profile::TrueColor));
        assert_eq!(
            strings(&text),
            ["2026-11-13T09:04:05+0000 app[7]: a       b"]
        );
    }

    #[test]
    fn an_origin_is_sanitized_cut_and_on_one_line() {
        let mut line = service_line(b"m");
        line.origin = b"ev\x1b[31mil\nx".to_vec();
        assert_eq!(
            strings(&text(&line, &format(Profile::TrueColor))),
            ["2026-11-13T09:04:05+0000 evil: m"]
        );
        line.origin = vec![b'o'; 1000];
        let shown = strings(&text(&line, &format(Profile::Ascii)));
        assert_eq!(shown[0].matches('o').count(), 256);
    }

    fn colours(text: &Text<'_>) -> Vec<Color> {
        text.lines
            .iter()
            .flat_map(|l| &l.spans)
            .flat_map(|s| [s.style.fg, s.style.bg, s.style.underline_color])
            .flatten()
            .collect()
    }

    const COLOURED: &[u8] = b"\x1b[38;2;255;0;0mred\x1b[48;5;200mbg\x1b[32mgreen\x1b[0m";

    #[test]
    fn truecolor_keeps_the_logs_colours() {
        let text = text(&service_line(COLOURED), &format(Profile::TrueColor));
        let c = colours(&text);
        assert!(c.contains(&Color::Rgb(255, 0, 0)), "{c:?}");
        assert!(c.contains(&Color::Indexed(200)), "{c:?}");
        assert!(c.contains(&Color::Green), "{c:?}");
        assert_eq!(
            strings(&text),
            ["2026-11-13T09:04:05+0000 app[7]: redbggreen"]
        );
    }

    #[test]
    fn ansi256_has_no_rgb() {
        let c = colours(&text(&service_line(COLOURED), &format(Profile::Ansi256)));
        assert!(!c.iter().any(|c| matches!(c, Color::Rgb(..))), "{c:?}");
        assert!(c.contains(&Color::Indexed(196)), "{c:?}");
        assert!(c.contains(&Color::Indexed(200)), "{c:?}");
    }

    #[test]
    fn ansi_has_only_the_sixteen() {
        let c = colours(&text(&service_line(COLOURED), &format(Profile::Ansi)));
        assert!(
            !c.iter()
                .any(|c| matches!(c, Color::Rgb(..) | Color::Indexed(_))),
            "{c:?}"
        );
        assert_ne!(c, []);
    }

    /// Every colour of a style is brought down, its underline's too.
    #[test]
    fn a_styles_every_colour_fits_the_profile() {
        let rgb = Color::Rgb(250, 120, 30);
        let style = Style::new().fg(rgb).bg(rgb).underline_color(rgb);
        let down = style_in(style, Profile::Ansi256);
        for colour in [down.fg, down.bg, down.underline_color] {
            assert!(matches!(colour, Some(Color::Indexed(_))), "{down:?}");
        }
        assert_eq!(style_in(style, Profile::TrueColor), style);
    }

    #[test]
    fn ascii_has_no_colour_at_all() {
        let mut f = format(Profile::Ascii);
        f.record_cap = 8;
        let text = text(&service_line(COLOURED), &f);
        assert_eq!(colours(&text), []);
    }

    /// Every SGR colour form a log can send: the 8 and the bright 8,
    /// 256 colours (a basic one, a cube one, a grey), RGB, as foreground
    /// and background.
    const EVERY_FORM: &[u8] = b"\x1b[31ma\x1b[94mb\x1b[42mc\x1b[103md\x1b[0m\
\x1b[38;5;3me\x1b[38;5;100mf\x1b[48;5;240mg\x1b[0m\
\x1b[38;2;1;2;3mh\x1b[48;2;200;100;50mi\x1b[0m j";

    /// Log spans step down with the colour profile (tui spec, "Text
    /// path"), and so do sb's own colours on a line (the cut notice's
    /// Muted, a search hit's background): no RGB under ansi256, only the
    /// sixteen under ansi, nothing under ascii; under truecolor sb's own
    /// are RGB, never a named colour, and the log's pass as they came.
    #[test]
    fn every_colour_on_a_line_fits_the_profile() {
        let line = service_line(EVERY_FORM);
        for profile in [
            Profile::TrueColor,
            Profile::Ansi256,
            Profile::Ansi,
            Profile::Ascii,
        ] {
            let mut f = format(profile);
            // Past the cap: the notice is drawn, the colours still all in.
            f.record_cap = EVERY_FORM.len() - 1;
            let text = searched(&line, &f, "a", f.hit());
            let c = colours(&text);
            let fits = |c: &Color| match profile {
                Profile::TrueColor => true,
                Profile::Ansi256 => !matches!(c, Color::Rgb(..)),
                Profile::Ansi => !matches!(c, Color::Rgb(..) | Color::Indexed(_)),
                Profile::Ascii => false,
            };
            assert!(c.iter().all(fits), "{profile:?}: {c:?}");
            let notice = text.lines[0].spans.last().expect("the notice");
            assert!(notice.content.contains("bytes not shown"), "{notice:?}");
            match profile {
                Profile::TrueColor => {
                    assert!(
                        matches!(notice.style.fg, Some(Color::Rgb(..))),
                        "{notice:?}"
                    );
                    assert!(c.contains(&Color::Rgb(1, 2, 3)), "{c:?}");
                    assert!(c.contains(&Color::Indexed(100)), "{c:?}");
                }
                Profile::Ansi256 => {
                    assert!(
                        matches!(notice.style.fg, Some(Color::Indexed(_))),
                        "{notice:?}"
                    );
                }
                Profile::Ansi => assert!(notice.style.fg.is_some(), "{notice:?}"),
                Profile::Ascii => assert_eq!(c, []),
            }
            // Every log form keeps a colour, none dropped: `a` to `i`.
            if profile != Profile::Ascii {
                for letter in ["a", "b", "c", "d", "e", "f", "g", "h", "i"] {
                    let span = text.lines[0]
                        .spans
                        .iter()
                        .find(|s| s.content == letter)
                        .unwrap_or_else(|| panic!("{letter}: {text:?}"));
                    assert!(
                        span.style.fg.is_some() || span.style.bg.is_some(),
                        "{profile:?} {letter}: {span:?}"
                    );
                }
            }
        }
    }

    fn hit() -> Style {
        Style::new().bg(Color::Yellow)
    }

    /// Search reads the message only: not the time, the origin or the cut
    /// notice, and nothing past the record cap.
    #[test]
    fn search_reads_the_message_only() {
        let mut f = format(Profile::Ascii);
        f.record_cap = 16;
        let line = service_line(b"started app\napp again, app past the cap");
        assert_eq!(hits(&line, &f, "app"), 2, "app[7] isn't searched");
        assert_eq!(hits(&line, &f, "cap"), 0);
        assert_eq!(hits(&line, &f, "bytes"), 0, "nor the notice");
        assert_eq!(hits(&line, &f, ""), 0);
        let text = searched(&line, &f, "app", hit());
        let lit: Vec<String> = text
            .lines
            .iter()
            .flat_map(|l| &l.spans)
            .filter(|s| s.style == hit())
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(lit, ["app", "app"]);
    }

    /// With colour, a match across the log's own colour spans is lit
    /// whole, each piece keeping its colour beneath.
    #[test]
    fn a_match_across_the_logs_colours_is_lit_whole() {
        let line = service_line(b"\x1b[32m[Info]\x1b[0m Housekeeping");
        let text = searched(&line, &format(Profile::TrueColor), "info] house", hit());
        let lit: Vec<(String, Option<Color>)> = text.lines[0]
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(Color::Yellow))
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect();
        assert_eq!(
            lit,
            [
                ("Info]".to_string(), Some(Color::Green)),
                (" House".to_string(), Some(Color::Reset))
            ]
        );
        // No query, no highlight: `text` is the search with none.
        assert_eq!(
            strings(&super::text(&line, &format(Profile::TrueColor))),
            strings(&text)
        );
    }

    /// Where a match starts: its screen line in the entry, and what is
    /// drawn before it there (the time and origin on the first line,
    /// when shown).
    #[test]
    fn a_hits_lead_is_what_comes_before_it_on_its_line() {
        let mut f = format(Profile::Ascii);
        let line = service_line(b"two\none two");
        let place = |row, lead: &str, word: &str| Some((row, lead.to_owned(), word.to_owned()));
        assert_eq!(
            hit_lead(&line, &f, "two", 0),
            place(0, "2026-11-13T09:04:05+0000 app[7]: ", "two")
        );
        assert_eq!(hit_lead(&line, &f, "two", 1), place(1, "one ", "two"));
        assert_eq!(hit_lead(&line, &f, "two", 2), None);
        f.meta = false;
        assert_eq!(hit_lead(&line, &f, "two", 0), place(0, "", "two"));
        let inside = service_line(b"a xxneedle-yy b");
        assert_eq!(
            hit_lead(&inside, &f, "needle", 0),
            place(0, "a xx", "needle-yy")
        );
    }

    #[test]
    fn a_hit_is_the_themes_search_hit_background() {
        let f = format(Profile::TrueColor);
        assert!(f.bg(Role::SearchHit).bg.is_some());
        assert_eq!(f.bg(Role::SearchHit).fg, None);
        assert_eq!(format(Profile::Ascii).bg(Role::SearchHit), Style::new());
    }

    /// A match is lit by the `SearchHit` background under truecolor and
    /// ansi256, and in reverse video under ansi and ascii (the maintainer,
    /// 2026-11-14): ansi's pick, yellow, hid a yellow span, and ascii has
    /// no colour to light it with.
    #[test]
    fn a_hit_is_the_search_hit_background_or_reverse_video() {
        for profile in [Profile::TrueColor, Profile::Ansi256] {
            let f = format(profile);
            assert_eq!(f.hit(), f.bg(Role::SearchHit), "{profile:?}");
            assert!(f.hit().bg.is_some(), "{profile:?}");
            assert_eq!(f.hit().fg, None, "{profile:?}");
            assert!(f.hit().add_modifier.is_empty(), "{profile:?}");
        }
        for profile in [Profile::Ansi, Profile::Ascii] {
            assert_eq!(
                format(profile).hit(),
                Style::new().add_modifier(Modifier::REVERSED),
                "{profile:?}"
            );
        }
    }

    /// Under ascii a match is reversed, the text around it untouched.
    #[test]
    fn under_ascii_a_match_is_reversed() {
        let f = format(Profile::Ascii);
        let text = searched(&service_line(b"one two one"), &f, "one", f.hit());
        let lit: Vec<String> = text.lines[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(lit, ["one", "one"]);
    }

    /// Messages whose wrap-off text has a trap: colour across the
    /// prefix's end, tabs, wide and combining characters, a split
    /// character, newlines inside string sequences, C1 codes, carriage
    /// returns that take a line back or end it.
    fn tricky() -> Vec<Vec<u8>> {
        let mut colours = Vec::new();
        for i in 0..600 {
            colours.extend_from_slice(format!("\x1b[3{}mword{i}\x1b[0m ", i % 8).as_bytes());
        }
        let mut lines = Vec::new();
        for i in 0..30 {
            lines.extend_from_slice(format!("{i:02} {}\n", "x".repeat(300)).as_bytes());
        }
        vec![
            format!("m7 {}", "lorem ipsum ".repeat(420)).into_bytes(),
            colours,
            lines,
            [vec![b'x'; 3000], b"\rtaken back".to_vec()].concat(),
            [vec![b'x'; 3000], b"\r\r".to_vec(), vec![b'y'; 50]].concat(),
            "crlf line\r\n".repeat(400).into_bytes(),
            "a\tbb\tccc\t".repeat(400).into_bytes(),
            "日本語テキスト ".repeat(300).into_bytes(),
            "e\u{301}".repeat(2000).into_bytes(),
            "é".repeat(2500).into_bytes(),
            b"osc \x1b]0;a\nb\x07 after\nnext".to_vec(),
            b"dcs \x1bPq\nx\x1b\\ after".to_vec(),
            b"sos \x1bX\n\x1b\\ pm \x1b^\n\x1b\\ apc \x1b_\n\x1b\\ end".to_vec(),
            "c1 osc \u{9d}0;t\nt\u{7} after".into(),
            "nel at the end\n\u{85}".into(),
            b"csi \x1b[3\n1m split\n".to_vec(),
            b"bell after\n\x07".to_vec(),
            b"\n".to_vec(),
            Vec::new(),
            b"a\n\nb\n\n".to_vec(),
        ]
    }

    /// Without building the text, the wrap-off count of screen lines is
    /// the text's, colour on and off, cut at the record cap too.
    #[test]
    fn screen_lines_are_the_texts_lines() {
        for profile in [Profile::Ascii, Profile::TrueColor] {
            let mut f = format(profile);
            for cap in [RECORD_CAP, 7] {
                f.record_cap = cap;
                for message in tricky() {
                    let line = service_line(&message);
                    assert_eq!(
                        screen_lines(&line, &f),
                        text(&line, &f).lines.len(),
                        "{profile:?} cap {cap}: {:?}",
                        String::from_utf8_lossy(&message)
                    );
                }
            }
        }
    }

    /// `rows` rows of `text` from column `column`, `width` wide, as a
    /// window with wrap off draws them.
    fn window(text: Text<'static>, rows: u16, column: u16, width: u16) -> ratatui::buffer::Buffer {
        use ratatui::widgets::Widget as _;
        let area = ratatui::layout::Rect::new(0, 0, width, rows);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        ratatui::widgets::Paragraph::new(text)
            .scroll((0, column))
            .render(area, &mut buffer);
        buffer
    }

    /// A window with wrap off draws `within`'s text as it draws the
    /// whole text: every row, column and style, with a search lit, at
    /// the left, inside and at the far end of the lines.
    #[test]
    fn a_window_draws_within_as_the_whole_text() {
        let windows = [
            (1, 0, 240),
            (5, 7, 40),
            (70, 200, 240),
            (3, 1000, 80),
            (1, 4990, 240),
        ];
        for profile in [Profile::Ascii, Profile::TrueColor] {
            let mut f = format(profile);
            for meta in [true, false] {
                f.meta = meta;
                for message in tricky() {
                    let line = service_line(&message);
                    for query in ["", "ipsum", "word1"] {
                        let whole = searched(&line, &f, query, hit());
                        for (rows, column, width) in windows {
                            let part = within(
                                &line,
                                &f,
                                query,
                                hit(),
                                usize::from(rows),
                                usize::from(column + width),
                            )
                            .0;
                            assert_eq!(
                                window(part, rows, column, width),
                                window(whole.clone(), rows, column, width),
                                "{profile:?} meta {meta} {query:?} {rows}x{width} at {column}: {:?}",
                                String::from_utf8_lossy(&message)
                            );
                        }
                    }
                }
            }
        }
    }

    /// `within` builds a long line only as far as the window reaches,
    /// further for a search, and whole when a carriage return could take
    /// the window's text back or the window reaches the end.
    #[test]
    fn within_builds_only_what_the_window_reaches() {
        let f = format(Profile::Ascii);
        let width = |text: &Text<'_>| text.lines.iter().map(TextLine::width).max();
        let long = service_line(format!("m7 {}", "lorem ipsum ".repeat(420)).as_bytes());
        let whole = width(&text(&long, &f)).expect("a line");
        let (part, whole_built) = within(&long, &f, "", hit(), 1, 240);
        assert!(!whole_built);
        let part = width(&part).expect("a line");
        assert!((242..1000).contains(&part), "{part} of {whole}");
        let searched_part = width(&within(&long, &f, "ipsum", hit(), 1, 240).0).expect("a line");
        assert!(searched_part >= 240 + 12, "{searched_part}");
        let (all, whole_built) = within(&long, &f, "", hit(), 1, 6000);
        assert_eq!((width(&all), whole_built), (Some(whole), true));
        let back = service_line(&[vec![b'x'; 3000], b"\rtaken back".to_vec()].concat());
        assert_eq!(
            strings(&within(&back, &f, "", hit(), 1, 240).0),
            strings(&text(&back, &f))
        );
        // The prefix's end right after a carriage return, text after it.
        let mut bare = format(Profile::Ascii);
        bare.meta = false;
        let end = 2 * (240 + 2);
        let at_end = service_line(&[vec![b'x'; end - 1], b"\ryyyy".to_vec()].concat());
        assert_eq!(
            strings(&within(&at_end, &bare, "", hit(), 1, 240).0),
            ["yyyy"]
        );
        // A carriage return still pending where the prefix ends: only a
        // C1 code (dropped before parsing) or the rest of an escape
        // sequence came after it, and the text after the end takes the
        // line back.
        let pending: [(&[u8], Profile); 3] = [
            (b"\r\xc2\x85", Profile::Ascii),
            (b"\x1b\r(B", Profile::Ascii),
            (b"\x1b[\r1m", Profile::TrueColor),
        ];
        for (tail, profile) in pending {
            let mut f = format(profile);
            f.meta = false;
            let end = 2 * (480 + 2);
            let message = [
                vec![b'a'; end - tail.len()],
                tail.to_vec(),
                vec![b'z'; 2000],
            ]
            .concat();
            let line = service_line(&message);
            assert_eq!(
                window(within(&line, &f, "", hit(), 1, 480).0, 1, 0, 240),
                window(text(&line, &f), 1, 0, 240),
                "{tail:?}"
            );
        }
        let crlf = service_line("crlf line\r\n".repeat(400).as_bytes());
        assert!(within(&crlf, &f, "", hit(), 2, 240).0.lines.len() < 100);
        let lines = service_line("short\n".repeat(400).as_bytes());
        let five = within(&lines, &f, "", hit(), 5, 240).0.lines.len();
        assert!((6..100).contains(&five), "{five}");
    }

    /// A match that starts at the window's right edge is lit there as in
    /// the whole text, where each character costs many bytes (its own
    /// colour), so the prefix's end is close to the window's.
    #[test]
    fn a_match_at_the_windows_edge_is_lit_whole() {
        let mut f = format(Profile::Ascii);
        f.meta = false;
        for width in 6..40_u16 {
            for before in usize::from(width) - 5..usize::from(width) {
                let plain = format!("{}needle{}", "a".repeat(before), "b".repeat(50));
                let mut message = String::new();
                for c in plain.chars() {
                    message.push_str("\x1b[31m");
                    message.push(c);
                }
                let line = service_line(message.as_bytes());
                let part = within(&line, &f, "needle", hit(), 1, usize::from(width)).0;
                assert_eq!(
                    window(part, 1, 0, width),
                    window(searched(&line, &f, "needle", hit()), 1, 0, width),
                    "{width} wide, the match at {before}"
                );
            }
        }
    }
}
