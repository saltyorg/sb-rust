//! Pasteable output (tui spec, "Pasteable output"; decided.md A-Q3) and
//! the inline fallback's lines ("The inline fallback"): log lines as the
//! screen shows them, as plain text for `s` (a file in the run log's
//! directory) and `p` (stdout), or plain or in colour for the stream.
//!
//! - Every line goes through [`line::text`], the viewer's one text path
//!   (`DisplayText`, the record cap, the profile's colours), so a file, a
//!   print and a stream hold what the screen shows.
//! - The file is std's `OpenOptions` with `create_new` and mode 0600
//!   (<https://doc.rust-lang.org/std/fs/struct.OpenOptions.html#method.create_new>,
//!   <https://doc.rust-lang.org/std/os/unix/fs/trait.OpenOptionsExt.html#tymethod.mode>):
//!   never an existing file, nor a link left in its place (`O_EXCL`).
//! - Colour on the stream is each span's style as anstyle renders it
//!   (<https://docs.rs/anstyle/1.0.14/anstyle/struct.Style.html#method.render>),
//!   through ratatui-core's conversions.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use ratatui::style::{Color, Style};
use sb_cli_core::theme::Theme;

use crate::line::{self, Format};
use crate::source::Line;
use crate::viewer::Kind;

/// What `s`'s notice and `p`'s stderr line ask of the user (tui spec,
/// "Pasteable output"): no automatic redaction.
pub const REVIEW: &str = "Review it for domains, IPs and tokens before sharing it.";

/// `lines` as plain text, one screen line per text line (colour off; the
/// time and origin as `format.meta` says), and how many log lines they
/// were.
pub(crate) fn plain<'a>(
    lines: impl IntoIterator<Item = &'a Line>,
    format: &Format,
) -> (String, usize) {
    let format = Format {
        theme: Theme::plain(),
        ..format.clone()
    };
    let mut text = String::new();
    let mut count = 0;
    for line in lines {
        text.push_str(&line::text(line, &format).to_string());
        text.push('\n');
        count += 1;
    }
    (text, count)
}

/// The saved file's name: `logs-<kind>-<name>-<YYYYMMDDTHHMMSSZ>.txt`,
/// the name keeping only `[A-Za-z0-9_.@-]`, every other character `_`.
pub(crate) fn file_name(kind: Kind, name: &str, now: Timestamp) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.@-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "logs-{}-{name}-{}.txt",
        kind.word(),
        now.strftime("%Y%m%dT%H%M%SZ")
    )
}

/// Write `text` to a new file `name` in `dir`, mode 0600.
///
/// # Errors
///
/// The I/O error when the file exists already or can't be created or
/// written.
pub(crate) fn save(dir: &Path, name: &str, text: &str) -> io::Result<PathBuf> {
    let path = dir.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(text.as_bytes())?;
    Ok(path)
}

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
fn ansi(style: Style) -> anstyle::Style {
    let colour = |c: Option<Color>| c.filter(|c| *c != Color::Reset).map(anstyle::Color::from);
    anstyle::Style::new()
        .fg_color(colour(style.fg))
        .bg_color(colour(style.bg))
        .underline_color(colour(style.underline_color))
        .effects(style.add_modifier.into())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use jiff::tz::TimeZone;
    use ratatui::style::Modifier;
    use sb_cli_core::theme::{Palette, Profile};

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
            position: testing::position(n),
        }
    }

    #[test]
    fn plain_text_is_the_screen_lines_without_colour() {
        let lines = [
            timed(1, b"\x1b[31mred\x1b[0m start"),
            timed(2, b"two\nlines"),
        ];
        let (text, count) = plain(&lines, &format(Profile::TrueColor));
        assert_eq!(
            text,
            "2026-11-13T09:04:05+0000 app[7]: red start\n\
             2026-11-13T09:04:05+0000 app[7]: two\nlines\n"
        );
        assert_eq!(count, 2);
    }

    #[test]
    fn plain_text_follows_t_and_keeps_the_cut_notice() {
        let mut f = format(Profile::Ascii);
        f.meta = false;
        f.record_cap = 4;
        let (text, count) = plain(&[timed(1, b"abcdefgh")], &f);
        assert_eq!(text, "abcd … [4 bytes not shown]\n");
        assert_eq!(count, 1);
        assert_eq!(plain(&[], &f), (String::new(), 0));
    }

    #[test]
    fn the_file_name_keeps_only_safe_characters() {
        let now: Timestamp = "2026-11-13T09:04:05.5Z".parse().expect("a time");
        assert_eq!(
            file_name(Kind::Service, "saltbox_managed_app", now),
            "logs-service-saltbox_managed_app-20261113T090405Z.txt"
        );
        assert_eq!(
            file_name(Kind::Container, "a/b c:d.e-f@g é", now),
            "logs-container-a_b_c_d.e-f@g__-20261113T090405Z.txt"
        );
        assert_eq!(
            file_name(Kind::Container, "../x", now),
            "logs-container-.._x-20261113T090405Z.txt"
        );
    }

    #[test]
    fn a_save_is_a_new_file_of_mode_0600() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = save(dir.path(), "logs-x.txt", "a\nb\n").expect("saved");
        assert_eq!(path, dir.path().join("logs-x.txt"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "a\nb\n");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let again = save(dir.path(), "logs-x.txt", "c").expect_err("exists");
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "a\nb\n");
    }

    #[test]
    fn a_save_refuses_a_link_in_the_files_place() {
        let dir = tempfile::tempdir().expect("temp dir");
        let target = dir.path().join("target");
        std::os::unix::fs::symlink(&target, dir.path().join("logs-x.txt")).expect("link");
        let err = save(dir.path(), "logs-x.txt", "a").expect_err("a link");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(!target.exists());
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
