//! A line built from a resume place draws as the whole line does
//! (`Paragraph::scroll` of [`crate::line::searched`]), its styles that
//! began left of the place included, and the places make a rebuild
//! parse only near the window.

use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::widgets::{Paragraph, Widget as _};
use sb_cli_core::theme::{Palette, Profile, Theme};

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

fn hit() -> Style {
    Style::new().bg(Color::Yellow).add_modifier(Modifier::BOLD)
}

/// `text`'s first line drawn `width` wide, scrolled to `column`.
fn drawn(text: Text<'static>, column: usize, width: u16) -> Buffer {
    let area = Rect::new(0, 0, width, 1);
    let mut buffer = Buffer::empty(area);
    let column = u16::try_from(column).expect("a test column");
    Paragraph::new(text)
        .scroll((0, column))
        .render(area, &mut buffer);
    buffer
}

/// One-line messages whose styles carry across any place: colours that
/// last for many words, set in every form and reset whole and in part,
/// modifiers on and off, with tabs, wide, combining and joined
/// characters, a C1 code and invalid UTF-8 late in the line.
fn messages() -> Vec<Vec<u8>> {
    let sgr = [
        "\x1b[1;31m",
        "\x1b[22m",
        "\x1b[38;5;208m",
        "\x1b[48;2;10;20;30m",
        "\x1b[39m",
        "\x1b[4m",
        "\x1b[3;7m",
        "\x1b[23;27m",
        "\x1b[0m",
        "\x1b[m",
        "\x1b[9;2m",
        "\x1b[29;49m",
        "\x1b[24m",
        "\x1b[5m",
        "\x1b[25m",
        "\x1b[8m",
        "\x1b[28m",
        "\x1b[38:2::1:2:3m",
        "\x1b[94m",
        "\x1b[103m",
    ];
    let mut styled = String::new();
    for i in 0..400 {
        if i % 7 == 0 {
            styled.push_str(sgr[(i / 7) % sgr.len()]);
        }
        styled.push_str("word");
        styled.push_str(&i.to_string());
        styled.push(' ');
    }
    let per_word: String = (0..700)
        .map(|i| format!("\x1b[3{}mword{i} ", i % 8))
        .collect::<Vec<_>>()
        .concat();
    vec![
        format!("m7 {}", "lorem ipsum ".repeat(420)).into_bytes(),
        styled.into_bytes(),
        per_word.into_bytes(),
        format!("\x1b[1;32m{}", "日本語のログ ".repeat(360)).into_bytes(),
        "a\tbb\tccc\t\x1b[35mdddd\t".repeat(300).into_bytes(),
        "e\u{301} x\u{308}y 👩‍💻 🇩🇰 ｶﾞ ".repeat(300).into_bytes(),
        [
            "lorem ipsum ".repeat(300).as_bytes(),
            b"\xff\xfe broken ",
            "\u{9b}31m c1 ".as_bytes(),
            "lorem ipsum ".repeat(100).as_bytes(),
        ]
        .concat(),
        format!("{}\n", "\x1b[33mtrailing newline ".repeat(200)).into_bytes(),
        // Few bytes a column, then many: a parse from places near the
        // start falls short and is done again longer.
        [
            "lorem ipsum ".repeat(250),
            (0..3000)
                .map(|i| {
                    format!(
                        "\x1b[3{}m{}",
                        i % 8,
                        char::from(b'a' + u8::try_from(i % 26).expect("a letter"))
                    )
                })
                .collect::<Vec<_>>()
                .concat(),
        ]
        .concat()
        .into_bytes(),
    ]
}

/// Every window from `far`'s left column through its last draws as the
/// whole line scrolled there, for a few columns across that range.
fn draws_as_the_whole(far: &Far, whole: &Text<'static>, width: u16, label: &str) {
    let last = far
        .columns
        .min(whole.lines[0].width())
        .saturating_sub(usize::from(width));
    let mut columns = vec![far.left, far.left + 7, last];
    columns.retain(|&c| c >= far.left && c <= last.max(far.left));
    for column in columns {
        assert_eq!(
            drawn(far.text.clone(), column - far.from, width),
            drawn(whole.clone(), column, width),
            "{label}: at {column}, from {}, left {}",
            far.from,
            far.left
        );
    }
}

/// A line built far right, from nothing and then again from the places
/// the first build kept, draws exactly as the whole line does, at its
/// left column and across what it holds: colour off, in sixteen colours
/// and in truecolor, with the time and origin and without, searched and
/// not, in a wide window and a narrow one.
#[test]
fn a_resumed_line_draws_as_the_whole_text() {
    let profiles = [Profile::Ascii, Profile::Ansi, Profile::TrueColor];
    for profile in profiles {
        for meta in [true, false] {
            let mut f = format(profile);
            f.meta = meta;
            for message in messages() {
                let line = service_line(&message);
                for query in ["", "word1", "Word", "ログ"] {
                    let whole = line::searched(&line, &f, query, hit());
                    let label = format!(
                        "{profile:?} meta {meta} {query:?} {:?}",
                        String::from_utf8_lossy(&message[..40])
                    );
                    for width in [240, 7] {
                        let mut resumes = Resumes::new(&line, &f);
                        // First far right, then nearer, then further:
                        // the second and third start from kept places.
                        for column in [3000, 1000, 1004, 30, 3990] {
                            let reach = column + usize::from(width) * 5 / 4;
                            let left = column.saturating_sub(usize::from(width) / 8);
                            let Some(far) =
                                far(&line, &f, query, hit(), &mut resumes, (left, reach), width)
                            else {
                                // Only a search on a line with joined
                                // graphemes is built from its start.
                                assert!(
                                    !query.is_empty() && resumes.message.joins,
                                    "{label}: resumable"
                                );
                                continue;
                            };
                            assert!(far.left <= column, "{label}: {far:?}");
                            assert!(far.columns >= reach, "{label}: {far:?}");
                            draws_as_the_whole(
                                &far,
                                &whole,
                                width,
                                &format!("{label} {width} wide"),
                            );
                        }
                    }
                }
            }
        }
    }
}

/// With nothing kept a build parses the line from its start; from the
/// places a build kept, a build near them parses only from near the
/// window, wherever the window is.
#[test]
fn kept_places_make_a_rebuild_parse_near_the_window() {
    let f = format(Profile::TrueColor);
    let line = service_line(format!("m7 {}", "lorem ipsum ".repeat(420)).as_bytes());
    let mut resumes = Resumes::new(&line, &f);
    let first = far(&line, &f, "", hit(), &mut resumes, (4000, 4300), 240).expect("resumable");
    assert!(first.parsed >= 4300, "{first:?}");
    assert!(first.text.lines[0].width() < 300 + 2 * EVERY, "{first:?}");
    let points = resumes.points.len();
    assert!(points >= 4300 / EVERY / 2, "{resumes:?}");
    assert!(resumes.size() >= points * size_of::<Point>());
    // A place is at most 48 bytes ([`EVERY`]'s reckoning).
    assert!(size_of::<Point>() <= 48, "{}", size_of::<Point>());
    for (left, reach) in [(4000, 4300), (2000, 2300), (4500, 4800)] {
        let again = far(&line, &f, "", hit(), &mut resumes, (left, reach), 240).expect("resumable");
        // About the columns from the place to the window's reach, at the
        // bytes a column the places tell, an eighth more.
        assert!(
            again.parsed <= (reach - left + 2 * EVERY) * 9 / 8 + 16,
            "{left}: {again:?}"
        );
        assert!(
            again.from <= left && again.from + 2 * EVERY >= left,
            "{again:?}"
        );
    }
    // The places stay in order.
    assert!(
        resumes
            .points
            .windows(2)
            .all(|w| w[0].byte < w[1].byte && w[0].column < w[1].column)
    );
}

/// Places fall only at a grapheme's start: a line of letters each with
/// a combining mark, or of joined emoji, still draws right from each.
#[test]
fn places_are_at_grapheme_starts() {
    let f = format(Profile::Ascii);
    for message in [
        "e\u{301}".repeat(3000),
        "👩‍💻".repeat(1500),
        "x\u{200d}".repeat(3000),
    ] {
        let line = service_line(message.as_bytes());
        let mut resumes = Resumes::new(&line, &f);
        let whole = line::searched(&line, &f, "", hit());
        for column in [2000, 1500, 2100] {
            let far = far(
                &line,
                &f,
                "",
                hit(),
                &mut resumes,
                (column, column + 300),
                240,
            )
            .expect("resumable");
            let label: String = message.chars().take(4).collect();
            draws_as_the_whole(&far, &whole, 240, &label);
        }
        let starts: Vec<usize> = Span::raw(message.as_str())
            .styled_graphemes(Style::new())
            .map(|g| offset(&message, g.symbol))
            .collect();
        assert!(
            resumes
                .points
                .iter()
                .all(|p| starts.binary_search(&p.byte).is_ok()),
            "{:?}",
            resumes.points
        );
    }
}

/// A line is built from its start where a place can't be trusted or
/// can't be found: several screen lines, a carriage return that takes
/// the line back, a query that can overlap itself, a window narrower
/// than a character in it.
#[test]
fn some_lines_are_built_from_their_start() {
    let f = format(Profile::TrueColor);
    let long = "lorem ipsum ".repeat(420);
    let build = |message: &str, query: &str, width: u16| {
        let line = service_line(message.as_bytes());
        let mut resumes = Resumes::new(&line, &f);
        far(&line, &f, query, hit(), &mut resumes, (2000, 2300), width).is_some()
    };
    assert!(build(&long, "", 240));
    assert!(build(&format!("{long}\n"), "", 240));
    assert!(build(&format!("{long}\r\n"), "", 240));
    assert!(!build(&format!("{long}\n{long}"), "", 240));
    assert!(!build(&format!("{long}\r{long}"), "", 240));
    assert!(!build(&format!("x\r\x1b[1m{long}"), "", 240));
    for query in ["aa", "abab", "ana", "LoL"] {
        assert!(!build(&long, query, 240), "{query}");
    }
    for query in ["lorem", "ipsum ", "Lorem", "ab", "aB", "Aa"] {
        assert!(build(&long, query, 240), "{query}");
    }
    // A search on text with no joiner, CJK or not, is resumed.
    assert!(build(&"日本 ".repeat(2000), "日本", 240));
    assert!(build(&"é ".repeat(2000), "é", 240));
    assert!(!build(&"日本".repeat(2000), "", 1));
    assert!(build(&long, "", 1));
    assert!(build(&"日本".repeat(2000), "", 2));
    // A place made with colour off holds no style: a change to colour
    // drops it.
    let line = service_line(format!("\x1b[31m{long}").as_bytes());
    let mut resumes = Resumes::new(&line, &format(Profile::Ascii));
    far(
        &line,
        &format(Profile::Ascii),
        "",
        hit(),
        &mut resumes,
        (2000, 2300),
        240,
    )
    .expect("resumable");
    assert_ne!(resumes.points, []);
    let coloured = far(&line, &f, "", hit(), &mut resumes, (2000, 2300), 240).expect("resumable");
    assert!(resumes.colour);
    assert!(
        coloured.text.lines[0]
            .spans
            .iter()
            .all(|s| s.style.fg == Some(Color::Red)),
        "{coloured:?}"
    );
}

/// Places made for one format's colour or record cap are dropped for
/// another's: their styles or bytes would be another text's.
#[test]
fn places_follow_the_colour_and_the_cap() {
    let line = service_line(format!("\x1b[31m{}", "lorem ipsum ".repeat(420)).as_bytes());
    let mut f = format(Profile::TrueColor);
    let mut resumes = Resumes::new(&line, &f);
    far(&line, &f, "", hit(), &mut resumes, (4000, 4300), 240).expect("resumable");
    assert!(resumes.points.iter().any(|p| p.byte > 3000));
    f.record_cap = 3000;
    let cut = far(&line, &f, "", hit(), &mut resumes, (2000, 2300), 240).expect("resumable");
    assert!(resumes.points.iter().all(|p| p.byte <= 3000), "{resumes:?}");
    draws_as_the_whole(&cut, &line::searched(&line, &f, "", hit()), 240, "cap");
}

/// A record over the cap ends with its notice when the build reaches
/// its end, as the whole text does.
#[test]
fn the_cut_notice_ends_a_build_that_reaches_the_end() {
    let mut f = format(Profile::TrueColor);
    f.record_cap = 3000;
    let line = service_line("lorem ipsum ".repeat(420).as_bytes());
    let whole = line::searched(&line, &f, "", hit());
    let mut resumes = Resumes::new(&line, &f);
    let end = far(&line, &f, "", hit(), &mut resumes, (2900, 3200), 240).expect("resumable");
    assert_eq!(end.columns, usize::MAX);
    let text: String = end.text.lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(text.ends_with("bytes not shown]"), "{text}");
    draws_as_the_whole(&end, &whole, 240, "cut");
    let before = far(&line, &f, "", hit(), &mut resumes, (2000, 2300), 240).expect("resumable");
    assert!(before.columns < usize::MAX);
    // From nothing, the parse reaches the end (twice the columns in
    // bytes), but the text is cut past the window, without the notice.
    let mut fresh = Resumes::new(&line, &f);
    let first = far(&line, &f, "", hit(), &mut fresh, (1500, 1800), 240).expect("resumable");
    assert_eq!(first.parsed, 3000, "the whole record parsed");
    let text: String = first.text.lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        first.columns < usize::MAX && !text.contains("not shown"),
        "{text}"
    );
}

/// Only a query some start of which is also its end can overlap itself.
#[test]
fn a_query_overlaps_itself_when_a_start_is_an_end() {
    for (query, overlaps_itself) in [
        ("aa", true),
        ("aba", true),
        ("abcab", true),
        ("abc", false),
        ("a", false),
        ("", false),
        ("Aa", false),
        ("AA", true),
        ("ÉÉ", true),
        ("éÉ", false),
    ] {
        assert_eq!(
            overlaps(query, &query.to_lowercase()),
            overlaps_itself,
            "{query}"
        );
    }
}

/// A search for part of a grapheme (an emoji's base before its variation
/// selector, a keycap's digit) lights the part, which ratatui then
/// measures apart from the rest: the line is built from its start.
#[test]
fn a_search_inside_a_grapheme_builds_from_the_start() {
    for (message, query) in [
        ("warn \u{26a0}\u{fe0f} done ", "\u{26a0}"),
        ("dev \u{1f469}\u{200d}\u{1f4bb} ok ", "\u{1f4bb}"),
        ("thumb \u{1f44d}\u{1f3fd} ok ", "\u{1f44d}"),
        ("key 1\u{fe0f}\u{20e3} ok ", "1"),
    ] {
        let f = format(Profile::TrueColor);
        let line = service_line(message.repeat(400).as_bytes());
        let mut resumes = Resumes::new(&line, &f);
        assert!(
            far(&line, &f, query, hit(), &mut resumes, (2000, 2150), 120).is_none(),
            "{message:?} {query:?}"
        );
        // Without a search it is built far right, and draws right.
        let far = far(&line, &f, "", hit(), &mut resumes, (2000, 2150), 120).expect("resumable");
        draws_as_the_whole(&far, &line::searched(&line, &f, "", hit()), 120, message);
    }
}

/// A parse whose end splits a character keeps no place at it: the
/// character may join the one before it (a keycap's marks after a digit,
/// a skin tone), so the windows built from every place kept draw right.
#[test]
fn a_parse_end_inside_a_character_keeps_no_place_there() {
    let mut f = format(Profile::Ascii);
    f.meta = false;
    for filler in ["ab1\u{fe0f}\u{20e3}", "k1\u{fe0f}\u{20e3} ", "x\u{1f3fd}"] {
        let message = filler.repeat(2000);
        let line = service_line(message.as_bytes());
        let whole = line::searched(&line, &f, "", hit());
        let starts: Vec<usize> = Span::raw(message.as_str())
            .styled_graphemes(Style::new())
            .map(|g| offset(&message, g.symbol))
            .collect();
        for width in [240_u16, 61] {
            let mut resumes = Resumes::new(&line, &f);
            for column in (40..4000_usize)
                .rev()
                .step_by(3)
                .chain((40..4000).step_by(7))
            {
                let reach = column + usize::from(width) * 5 / 4;
                let left = column.saturating_sub(usize::from(width) / 8);
                let far = far(&line, &f, "", hit(), &mut resumes, (left, reach), width)
                    .expect("resumable");
                assert_eq!(
                    drawn(far.text.clone(), column - far.from, width),
                    drawn(whole.clone(), column, width),
                    "{filler:?} {width} wide at {column}"
                );
                assert!(
                    resumes
                        .points
                        .iter()
                        .all(|p| starts.binary_search(&p.byte).is_ok()),
                    "{filler:?} {width} wide at {column}: {resumes:?}"
                );
            }
        }
    }
}

/// A line's places made ahead for the whole line ([`scan`], the viewer's
/// background work) let its first build far right parse only near the
/// window, and it draws as the whole line does; a line that can't be
/// resumed gets none, and a whole line is scanned once.
#[test]
fn places_made_ahead_serve_the_first_build_far_right() {
    for (name, words) in [
        ("plain", "lorem ipsum ".repeat(420)),
        ("cjk", "日本語のログ ".repeat(360)),
        (
            "colour",
            (0..700)
                .map(|i| format!("\x1b[3{}mword{i} ", i % 8))
                .collect::<Vec<_>>()
                .concat(),
        ),
    ] {
        let f = format(Profile::TrueColor);
        let line = service_line(format!("m7 {words}").as_bytes());
        let mut resumes = Resumes::new(&line, &f);
        assert!(!resumes.is_whole());
        scan(line::shown(&line, &f).0, &mut resumes);
        assert!(resumes.is_whole(), "{name}");
        let points = resumes.points.len();
        scan(line::shown(&line, &f).0, &mut resumes);
        assert_eq!(resumes.points.len(), points, "{name}: scanned once");
        let whole = line::searched(&line, &f, "", hit());
        for column in [4000, 1000] {
            let far = far(
                &line,
                &f,
                "",
                hit(),
                &mut resumes,
                (column, column + 300),
                240,
            )
            .expect("resumable");
            assert!(
                far.parsed <= (300 + 2 * EVERY) * 2,
                "{name} at {column}: {far:?}"
            );
            draws_as_the_whole(&far, &whole, 240, name);
        }
    }
    // Several screen lines: nothing to scan.
    let f = format(Profile::TrueColor);
    let lines = service_line(format!("{}\ntwo", "lorem ipsum ".repeat(50)).as_bytes());
    let mut resumes = Resumes::new(&lines, &f);
    scan(line::shown(&lines, &f).0, &mut resumes);
    assert_eq!(resumes.points, []);
    assert!(resumes.is_whole());
}
