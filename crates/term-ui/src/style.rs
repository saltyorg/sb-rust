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

use sb_cli_core::theme::{Role, Theme};

/// `text` in `role`'s colour, or `text` unchanged with colour off (`on`
/// is the destination stream's choice) or under `ascii`. anstyle's own
/// pattern (<https://docs.rs/anstyle/1.0.14/anstyle/struct.Style.html>).
pub fn paint(theme: &Theme, role: Role, on: bool, text: &str) -> String {
    // A role without a colour gives an empty style, which renders as
    // nothing (`Theme::style` sets only the colour), so this agrees with
    // `table_colour`'s check.
    let style = theme.style(role);
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
pub fn table_colour(theme: &Theme, role: Role, on: bool) -> Option<tabled::settings::Color> {
    let style = theme.style(role);
    (on && theme.color(role).is_some()).then(|| {
        tabled::settings::Color::new(style.render().to_string(), style.render_reset().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_cli_core::theme::{Palette, Profile, Role, Theme};

    // Why not anstyle-parse's VTE parser: these only read what anstyle
    // itself wrote, and a `Perform` impl would outweigh the tests.

    /// The colour parameters of every SGR sequence in `text`, e.g.
    /// `["38;2;111;211;154"]` (bold and resets are left out).
    fn colour_codes(text: &str) -> Vec<String> {
        text.split('\x1b')
            .filter_map(|s| s.strip_prefix('['))
            .filter_map(|s| s.split_once('m').map(|(params, _)| params.to_string()))
            .filter(|params| !matches!(params.as_str(), "" | "0" | "1" | "2" | "22" | "39" | "49"))
            .collect()
    }

    /// Whether one SGR colour parameter list is of `profile`'s kind.
    fn of_kind(params: &str, profile: Profile) -> bool {
        let first: Vec<&str> = params.splitn(3, ';').collect();
        match (profile, first.as_slice()) {
            (Profile::TrueColor, ["38" | "48", "2", _]) => true,
            (Profile::Ansi256, ["38" | "48", "5", _]) => true,
            (Profile::Ansi, [code]) => matches!(
                code.parse::<u8>(),
                Ok(30..=37 | 40..=47 | 90..=97 | 100..=107)
            ),
            _ => false,
        }
    }

    /// Everything the adapters write for every role under `profile`.
    fn written(profile: Profile) -> String {
        let theme = Theme::resolve(&Palette::default(), profile);
        Role::ALL
            .into_iter()
            .map(|role| {
                let table = table_colour(&theme, role, true)
                    .map(|c| format!("{}x{}", c.get_prefix(), c.get_suffix()))
                    .unwrap_or_default();
                format!("{}{table}", paint(&theme, role, true, "x"))
            })
            .collect()
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
