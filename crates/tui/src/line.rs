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
use ratatui::style::{Color, Style};
use ratatui::text::{Line as TextLine, Span, Text};
use sb_cli_core::theme::{Profile, Role, Theme};
use sb_term_ui::DisplayText;

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

    fn colour(&self) -> bool {
        self.theme.profile() != Profile::Ascii
    }
}

/// `line` as screen text: one screen line per line of the message, the
/// time and origin before the first, the cut notice after the last.
#[must_use]
pub fn text(line: &Line, format: &Format) -> Text<'static> {
    // A byte cut: it can split a character (one U+FFFD before the notice)
    // or an SGR (dropped by `DisplayText`), both harmless.
    let cut = line.message.len().saturating_sub(format.record_cap);
    let shown = &line.message[..line.message.len() - cut];
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
}
