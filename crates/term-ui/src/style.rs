//! Theme adapters (term-ui spec, "Colour on every inline path"): a role
//! of cli-core's resolved theme becomes styled text or a tabled colour,
//! with no colour maths here (anstyle-lossy resolved the profile).
//!
//! Why not console's `Style`: console writes a bright 16-colour as
//! `38;5;8`-`15` (console 0.16.6 `src/utils.rs:809-810`, `48;5;` at 820-821;
//! <https://github.com/console-rs/console/blob/0.16.6/src/utils.rs>), a
//! 256-colour code, and the `ansi` profile resolves several roles to
//! bright colours, so success criterion 2 would fail. anstyle writes
//! `90`-`97` and `100`-`107`, and indicatif and console measure text with
//! escape codes stripped (`console::measure_text_width`), so styled text
//! is drawn as is.

use anstyle::{Color, Style};
use sb_cli_core::theme::{Role, Theme};

/// `text` in `role`'s colour, or `text` unchanged with colour off (`on`
/// is the destination stream's choice) or under `ascii`.
#[must_use]
pub fn paint(theme: &Theme, role: Role, on: bool, text: &str) -> String {
    paint_colour(theme.color(role), on, text)
}

/// `text` in `colour`, one its caller resolved for the run's profile
/// with cli-core's `Profile::resolve` (motd.yml's own colours, motd
/// decision 5), or `text` unchanged with colour off or no colour. anstyle's
/// own pattern (<https://docs.rs/anstyle/1.0.14/anstyle/struct.Style.html>).
#[must_use]
pub fn paint_colour(colour: Option<Color>, on: bool, text: &str) -> String {
    // No colour gives an empty style, which renders as nothing, so this
    // agrees with `table_colour`'s check.
    let style = Style::new().fg_color(colour);
    if on {
        format!("{style}{text}{style:#}")
    } else {
        text.to_string()
    }
}

/// `role`'s colour for tabled (as foreground), or none with colour off or
/// under `ascii`. tabled writes whatever codes it is given
/// (<https://docs.rs/tabled/0.22.0/tabled/settings/struct.Color.html#method.new>),
/// so the same anstyle rendering keeps the profile's kind.
#[must_use]
pub fn table_colour(theme: &Theme, role: Role, on: bool) -> Option<tabled::settings::Color> {
    let style = theme.style(role);
    (on && theme.color(role).is_some()).then(|| {
        tabled::settings::Color::new(style.render().to_string(), style.render_reset().to_string())
    })
}

/// A one-line usage bar (term-ui spec, adapters, the motd amendment):
/// `█` for the filled part, `round(width × ratio)` cells in `role`, and
/// `░` for the rest in Faint, each through [`paint`]. The rounding is
/// bubbles' `ViewAs` (`math.Round`, half away from zero, the ratio
/// clamped to 0..=1), so sb-go's MOTD bars and these fill the same
/// cells
/// (<https://github.com/charmbracelet/bubbles/blob/v2.2.1/progress/progress.go>).
///
/// Why not indicatif: it renders a bar only through a draw target
/// (`ProgressStyle::format_state` is `pub(crate)`, indicatif 0.18.6
/// `src/style.rs:214`), and its stdout target draws nothing when stdout
/// isn't a terminal (`src/draw_target.rs:80`), which is the login's
/// case under `pam_motd`; and it rounds the fill down (`format_bar`,
/// `src/style.rs:174-176`), one cell short of sb-go's at a half
/// (<https://github.com/console-rs/indicatif/blob/0.18.6/src/style.rs>).
/// Why not ratatui's `LineGauge`: ratatui isn't a term-ui dependency, its
/// inline viewport asks the terminal for the cursor (ratatui-core 0.1.2
/// `src/terminal/inline.rs:396`), which a pipe can't answer, and
/// rendering into a `Buffer` and turning cells into SGR text would be
/// rendering code of our own (motd decision 8).
#[must_use]
pub fn bar(theme: &Theme, role: Role, on: bool, ratio: f64, width: usize) -> String {
    bar_colour(theme, theme.color(role), on, ratio, width)
}

/// [`bar`] with the filled part in `colour`, one its caller resolved
/// (motd.yml's `progress_bar` colours, motd decision 5); the rest stays
/// `theme`'s Faint.
#[must_use]
pub fn bar_colour(
    theme: &Theme,
    colour: Option<Color>,
    on: bool,
    ratio: f64,
    width: usize,
) -> String {
    let filled = filled_cells(ratio, width);
    let mut out = paint_colour(colour, on, &"█".repeat(filled));
    out.push_str(&paint(theme, Role::Faint, on, &"░".repeat(width - filled)));
    out
}

/// `round(width × ratio)`, half away from zero, within 0..=width: the
/// cells `k` from 1 with `k - 0.5 <= width × ratio`, which is exact in
/// `f64` and needs no float-to-integer cast.
fn filled_cells(ratio: f64, width: usize) -> usize {
    let cells = u32::try_from(width).unwrap_or(u32::MAX);
    let target = f64::from(cells) * ratio;
    (1..=cells)
        .take_while(|&k| f64::from(k) - 0.5 <= target)
        .count()
}

#[cfg(test)]
pub(crate) mod sgr {
    //! Reading the colour codes text carries, for the profile guard tests.
    use sb_cli_core::theme::Profile;

    // Why not anstyle-parse's VTE parser: these only read what anstyle
    // itself wrote, and a `Perform` impl would outweigh the tests.

    /// The colour parameters of every SGR sequence in `text`, e.g.
    /// `["38;2;111;211;154"]` (bold and resets are left out).
    pub(crate) fn colour_codes(text: &str) -> Vec<String> {
        text.split('\x1b')
            .filter_map(|s| s.strip_prefix('['))
            .filter_map(|s| s.split_once('m').map(|(params, _)| params.to_string()))
            .filter(|params| !matches!(params.as_str(), "" | "0" | "1" | "2" | "22" | "39" | "49"))
            .collect()
    }

    /// Whether one SGR colour parameter list is of `profile`'s kind.
    pub(crate) fn of_kind(params: &str, profile: Profile) -> bool {
        let first: Vec<&str> = params.splitn(3, ';').collect();
        match (profile, first.as_slice()) {
            (Profile::TrueColor, ["38" | "48", "2", _])
            | (Profile::Ansi256, ["38" | "48", "5", _]) => true,
            (Profile::Ansi, [code]) => matches!(
                code.parse::<u8>(),
                Ok(30..=37 | 40..=47 | 90..=97 | 100..=107)
            ),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sgr::{colour_codes, of_kind};
    use std::fmt::Write as _;

    use super::*;
    use sb_cli_core::theme::{Palette, Profile, Role, Theme};

    /// Everything the adapters write for every role under `profile`.
    fn written(profile: Profile) -> String {
        let theme = Theme::resolve(&Palette::default(), profile);
        Role::ALL.into_iter().fold(String::new(), |mut out, role| {
            let table = table_colour(&theme, role, true)
                .map_or_default(|c| format!("{}x{}", c.get_prefix(), c.get_suffix()));
            let _ = write!(out, "{}{table}", paint(&theme, role, true, "x"));
            out
        })
    }

    /// bubbles' rounding: half away from zero, clamped to the width.
    #[test]
    fn bar_fills_round_width_times_ratio_cells() {
        let cells = |ratio: f64, width| filled_cells(ratio, width);
        assert_eq!(cells(0.0, 50), 0);
        assert_eq!(cells(1.0, 50), 50);
        assert_eq!(cells(0.5, 50), 25);
        // 50 × 0.01 is 0.5 in f64, which rounds up, as Go's math.Round.
        assert_eq!(cells(f64::from(1_u8) / 100.0, 50), 1);
        assert_eq!(cells(f64::from(3_u8) / 100.0, 50), 2); // 1.5
        assert_eq!(cells(0.029, 50), 1); // 1.45
        assert_eq!(cells(0.99, 50), 50); // 49.5
        assert_eq!(cells(0.98, 50), 49);
        assert_eq!(cells(1.5, 10), 10);
        assert_eq!(cells(-0.5, 10), 0);
        assert_eq!(cells(f64::NAN, 10), 0);
        assert_eq!(cells(0.5, 0), 0);
    }

    #[test]
    fn bar_draws_filled_cells_in_the_role_and_the_rest_in_faint() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        assert_eq!(
            bar(&theme, Role::Ok, true, 0.4, 10),
            format!(
                "{}{}",
                paint(&theme, Role::Ok, true, "████"),
                paint(&theme, Role::Faint, true, "░░░░░░")
            )
        );
        assert_ne!(
            paint(&theme, Role::Ok, true, "█"),
            paint(&theme, Role::Faint, true, "█")
        );
        assert_eq!(bar(&theme, Role::Problem, false, 0.25, 4), "█░░░");
        assert_eq!(bar(&Theme::plain(), Role::Ok, true, 1.0, 3), "███");
    }

    /// A colour of the caller's own paints as a role's does; none, or
    /// colour off, leaves the text as it is.
    #[test]
    fn a_resolved_colour_paints_as_a_roles_does() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let pink = Some(Color::Rgb(anstyle::RgbColor(0xFF, 0x79, 0xD0)));
        assert_eq!(
            paint_colour(pink, true, "k"),
            "\x1b[38;2;255;121;208mk\x1b[0m"
        );
        assert_eq!(
            paint_colour(theme.color(Role::Ok), true, "k"),
            paint(&theme, Role::Ok, true, "k")
        );
        assert_eq!(paint_colour(pink, false, "k"), "k");
        assert_eq!(paint_colour(None, true, "k"), "k");
        assert_eq!(
            bar_colour(&theme, pink, true, 0.5, 4),
            format!(
                "{}{}",
                paint_colour(pink, true, "██"),
                paint(&theme, Role::Faint, true, "░░")
            )
        );
        assert_eq!(bar_colour(&theme, pink, false, 0.5, 4), "██░░");
    }

    #[test]
    fn every_colour_code_is_of_the_profiles_kind() {
        for profile in [Profile::TrueColor, Profile::Ansi256, Profile::Ansi] {
            let codes = colour_codes(&written(profile));
            assert!(!codes.is_empty(), "{profile:?} wrote no colour");
            for params in codes {
                assert!(of_kind(&params, profile), "{profile:?} wrote {params:?}");
            }
        }
    }

    #[test]
    fn every_coloured_role_has_a_table_colour() {
        let theme = Theme::resolve(&Palette::default(), Profile::Ansi256);
        for role in Role::ALL {
            let colour = table_colour(&theme, role, true).expect("a table colour");
            assert!(colour.get_prefix().starts_with("\x1b["), "{role:?}");
        }
    }

    #[test]
    fn ascii_writes_no_escape_codes() {
        assert!(!written(Profile::Ascii).contains('\x1b'));
    }

    #[test]
    fn colour_off_writes_no_escape_codes() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        for role in Role::ALL {
            assert_eq!(paint(&theme, role, false, "x"), "x");
            assert!(table_colour(&theme, role, false).is_none());
        }
    }

    #[test]
    fn ansi_profile_exercises_bright_colours() {
        // console would write these as 38;5;8-15 (why anstyle paints).
        let theme = Theme::resolve(&Palette::default(), Profile::Ansi);
        let bright = Role::ALL
            .into_iter()
            .flat_map(|role| colour_codes(&paint(&theme, role, true, "x")))
            .filter(|params| params.starts_with('9') || params.starts_with("10"))
            .count();
        assert!(
            bright > 0,
            "the default palette has bright colours under ansi"
        );
    }
}
