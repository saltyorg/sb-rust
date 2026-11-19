//! The inline fallback's lines (tui spec, "The inline fallback"): log
//! lines as the screen shows them, plain or in colour, for the stream a
//! piped stdout gets.
//!
//! - Every line goes through [`line::text`], the viewer's one text path
//!   (`DisplayText`, the record cap, the profile's colours), so the stream
//!   holds what the screen shows.
//! - Colour on the stream is each span's style as anstyle renders it
//!   (<https://docs.rs/anstyle/1.0.14/anstyle/struct.Style.html#method.render>),
//!   through ratatui-core's conversions.

use std::io::{self, Write};

use ratatui::style::{Color, Style};

use crate::line::{self, Format};
use crate::source::Line;

/// `line` on `out` as the stream writes it: its screen lines, each span in
/// its style (none under the plain theme).
///
/// # Errors
///
/// The write's error.
pub(crate) fn write_line(out: &mut impl Write, line: &Line, format: &Format) -> io::Result<()> {
    for screen_line in line::text(line, format).lines {
        for span in screen_line.spans {
            let style = ansi(span.style);
            write!(
                out,
                "{}{}{}",
                style.render(),
                span.content,
                style.render_reset()
            )?;
        }
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// `style` as anstyle's. `Reset` is no colour: ratatui-core's
/// `From<Color> for anstyle::Color` panics on it.
pub(crate) fn ansi(style: Style) -> anstyle::Style {
    let colour = |c: Option<Color>| c.filter(|c| *c != Color::Reset).map(anstyle::Color::from);
    anstyle::Style::new()
        .fg_color(colour(style.fg))
        .bg_color(colour(style.bg))
        .underline_color(colour(style.underline_color))
        .effects(style.add_modifier.into())
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;
    use ratatui::style::Modifier;
    use sb_cli_core::theme::{Palette, Profile, Theme};

    use super::*;
    use crate::testing;

    fn format(profile: Profile) -> Format {
        Format::new(TimeZone::UTC, Theme::resolve(&Palette::default(), profile))
    }

    fn timed(n: u64, message: &[u8]) -> Line {
        Line {
            time: Some("2026-11-13T09:04:05Z".parse().expect("a time")),
            origin: b"app[7]".to_vec(),
            message: message.to_vec(),
            cut: 0,
            position: testing::position(n),
        }
    }

    #[test]
    fn a_stream_line_is_plain_under_the_plain_theme() {
        let mut out = Vec::new();
        let f = Format::new(TimeZone::UTC, Theme::plain());
        write_line(&mut out, &timed(1, b"\x1b[31mred\x1b[0m\nnext"), &f).expect("written");
        assert_eq!(
            String::from_utf8(out).expect("utf-8"),
            "2026-11-13T09:04:05+0000 app[7]: red\nnext\n"
        );
    }

    #[test]
    fn a_stream_line_keeps_its_colours_under_colour() {
        let mut out = Vec::new();
        let f = format(Profile::Ansi);
        write_line(&mut out, &timed(1, b"\x1b[1;31mred\x1b[0m plain"), &f).expect("written");
        let out = String::from_utf8(out).expect("utf-8");
        assert_eq!(
            out,
            "2026-11-13T09:04:05+0000 app[7]: \x1b[1m\x1b[31mred\x1b[0m plain\n"
        );
    }

    #[test]
    fn reset_colours_are_no_colour() {
        let style = Style::new()
            .fg(Color::Reset)
            .bg(Color::Reset)
            .underline_color(Color::Reset)
            .add_modifier(Modifier::ITALIC);
        assert_eq!(
            ansi(style),
            anstyle::Style::new().effects(anstyle::Effects::ITALIC)
        );
        let style = Style::new()
            .fg(Color::Red)
            .bg(Color::Blue)
            .underline_color(Color::Green);
        assert_eq!(
            ansi(style),
            anstyle::Style::new()
                .fg_color(Some(anstyle::AnsiColor::Red.into()))
                .bg_color(Some(anstyle::AnsiColor::Blue.into()))
                .underline_color(Some(anstyle::AnsiColor::Green.into()))
        );
    }
}
