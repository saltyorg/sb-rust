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

/// The most of an origin shown: a service sets its own identifier, so it
/// is untrusted and unbounded too.
const ORIGIN_CAP: usize = 256;

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
#[derive(Clone, Debug)]
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

    fn colour(&self) -> bool {
        self.theme.profile() != Profile::Ascii
    }
}

/// `line` as screen text: one screen line per line of the message, the
/// time and origin before the first, the cut notice after the last.
#[must_use]
pub fn text(line: &Line, format: &Format) -> Text<'static> {
    searched(line, format, "", Style::new())
}

/// The message's bytes shown: cut at the record cap, and how many went.
fn shown<'a>(line: &'a Line, format: &Format) -> (&'a [u8], usize) {
    // A byte cut: it can split a character (one U+FFFD before the notice)
    // or an SGR (dropped by `DisplayText`), both harmless.
    let cut = line.message.len().saturating_sub(format.record_cap);
    (&line.message[..line.message.len() - cut], cut)
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
fn meta(line: &Line, format: &Format) -> String {
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
fn local(time: Timestamp, zone: &TimeZone) -> String {
    time.to_zoned(zone.clone())
        .strftime("%Y-%m-%dT%H:%M:%S%z")
        .to_string()
}

/// `text`'s colours in `profile`: RGB and 256 colours brought down by
/// anstyle-lossy, as the palette is.
fn lossy(mut text: Text<'static>, profile: Profile) -> Text<'static> {
    for line in &mut text.lines {
        for span in &mut line.spans {
            span.style = Style {
                fg: span.style.fg.map(|c| colour_in(c, profile)),
                bg: span.style.bg.map(|c| colour_in(c, profile)),
                underline_color: span.style.underline_color.map(|c| colour_in(c, profile)),
                ..span.style
            };
        }
    }
    text
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

    #[test]
    fn ascii_has_no_colour_at_all() {
        let mut f = format(Profile::Ascii);
        f.record_cap = 8;
        let text = text(&service_line(COLOURED), &f);
        assert_eq!(colours(&text), []);
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
}
