//! sb's colours (spec: startup step 6; ADR-002).
//!
//! One role-named RGB palette (the one the prototypes were reviewed with,
//! proto-theme) is resolved once at startup for the colour profile:
//! truecolor as is, or converted with anstyle-lossy, the anstream project's
//! conversion crate (https://docs.rs/anstyle-lossy/1.1.5/anstyle_lossy/).
//! The 16-colour palette of a terminal can't be known, so `ansi` matches
//! against anstyle-lossy's VGA palette. Output libraries take their
//! colours from the resolved [`Theme`]; each one's adapter comes with the
//! module that brings that library (term-ui, tui), not before.
//! `--color=never` and NO_COLOR still win: output goes through anstream.

use std::sync::OnceLock;

use anstyle::{Color, RgbColor, Style};

/// A colour's job, not its hue: screens share one meaning per role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Text,
    Muted,
    Faint,
    Border,
    Accent,
    OnAccent,
    Ok,
    Problem,
    Warn,
    Idle,
    Service,
    ServiceRule,
    Container,
    ContainerRule,
    AttentionRule,
    Highlight,
    Link,
    Code,
    CodeBg,
    Accent2,
    ButtonBg,
    ButtonDisabledBg,
    SearchHit,
    SelectedFg,
    SelectedBg,
}

impl Role {
    /// Every role, in palette order.
    pub const ALL: [Role; 25] = [
        Role::Text,
        Role::Muted,
        Role::Faint,
        Role::Border,
        Role::Accent,
        Role::OnAccent,
        Role::Ok,
        Role::Problem,
        Role::Warn,
        Role::Idle,
        Role::Service,
        Role::ServiceRule,
        Role::Container,
        Role::ContainerRule,
        Role::AttentionRule,
        Role::Highlight,
        Role::Link,
        Role::Code,
        Role::CodeBg,
        Role::Accent2,
        Role::ButtonBg,
        Role::ButtonDisabledBg,
        Role::SearchHit,
        Role::SelectedFg,
        Role::SelectedBg,
    ];

    /// The role's name in the theme file.
    pub fn name(self) -> &'static str {
        match self {
            Role::Text => "text",
            Role::Muted => "muted",
            Role::Faint => "faint",
            Role::Border => "border",
            Role::Accent => "accent",
            Role::OnAccent => "on_accent",
            Role::Ok => "ok",
            Role::Problem => "problem",
            Role::Warn => "warn",
            Role::Idle => "idle",
            Role::Service => "service",
            Role::ServiceRule => "service_rule",
            Role::Container => "container",
            Role::ContainerRule => "container_rule",
            Role::AttentionRule => "attention_rule",
            Role::Highlight => "highlight",
            Role::Link => "link",
            Role::Code => "code",
            Role::CodeBg => "code_bg",
            Role::Accent2 => "accent_2",
            Role::ButtonBg => "button_bg",
            Role::ButtonDisabledBg => "button_disabled_bg",
            Role::SearchHit => "search_hit",
            Role::SelectedFg => "selected_fg",
            Role::SelectedBg => "selected_bg",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// RGB values for every role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette([RgbColor; Role::ALL.len()]);

const fn rgb(hex: u32) -> RgbColor {
    RgbColor((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Default for Palette {
    /// The built-in `default` theme, for dark backgrounds.
    fn default() -> Self {
        Self([
            rgb(0xDCE3EC), // text
            rgb(0x8B95A7), // muted
            rgb(0x586174), // faint
            rgb(0x3A4558), // border
            rgb(0x5CCFE6), // accent
            rgb(0x0B1A24), // on_accent
            rgb(0x6FD39A), // ok
            rgb(0xFF6E7A), // problem
            rgb(0xF5B55B), // warn
            rgb(0x84A9C0), // idle
            rgb(0x7EA6FF), // service
            rgb(0x34466E), // service_rule
            rgb(0xC49BFF), // container
            rgb(0x4F3D6E), // container_rule
            rgb(0x6A2E36), // attention_rule
            rgb(0xE8C77A), // highlight
            rgb(0x7EA6FF), // link
            rgb(0xE8C77A), // code
            rgb(0x1C2430), // code_bg
            rgb(0xC49BFF), // accent_2
            rgb(0x2A3446), // button_bg
            rgb(0x1A2029), // button_disabled_bg
            rgb(0x4A3F12), // search_hit
            rgb(0xFFFFFF), // selected_fg
            rgb(0x24405E), // selected_bg
        ])
    }
}

impl Palette {
    pub fn get(&self, role: Role) -> RgbColor {
        self.0[role.index()]
    }
}

/// How many colours the terminal gets (sb-go's `SB_COLOR_PROFILE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    TrueColor,
    Ansi256,
    Ansi,
    Ascii,
}

impl Profile {
    /// sb-go's values; none or empty means truecolor; an unknown one means
    /// truecolor as in sb-go, and warns (sb-go is silent).
    pub fn parse(value: Option<&str>) -> (Self, Option<String>) {
        match value {
            // sb-go reads an empty value as unset (main.go: `!= ""`).
            None | Some("" | "truecolor") => (Profile::TrueColor, None),
            Some("ansi256") => (Profile::Ansi256, None),
            Some("ansi") => (Profile::Ansi, None),
            Some("ascii") => (Profile::Ascii, None),
            Some(other) => (
                Profile::TrueColor,
                Some(format!(
                    "unknown SB_COLOR_PROFILE {other:?} (truecolor, ansi256, ansi, ascii); using truecolor"
                )),
            ),
        }
    }

    /// One palette colour in this profile; `ascii` has none.
    pub fn resolve(self, colour: RgbColor) -> Option<Color> {
        match self {
            Profile::TrueColor => Some(Color::Rgb(colour)),
            Profile::Ansi256 => Some(Color::Ansi256(anstyle_lossy::rgb_to_xterm(colour))),
            Profile::Ansi => Some(Color::Ansi(anstyle_lossy::rgb_to_ansi(
                colour,
                anstyle_lossy::palette::VGA,
            ))),
            Profile::Ascii => None,
        }
    }
}

/// The palette resolved for one profile: what output uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme([Option<Color>; Role::ALL.len()]);

impl Theme {
    pub fn resolve(palette: &Palette, profile: Profile) -> Self {
        Self(palette.0.map(|colour| profile.resolve(colour)))
    }

    /// No colour at all (before startup installs the real theme).
    pub fn plain() -> Self {
        Self([None; Role::ALL.len()])
    }

    /// The role's resolved colour, for any use (backgrounds included).
    pub fn color(&self, role: Role) -> Option<Color> {
        self.0[role.index()]
    }

    /// A foreground style in the role's colour.
    pub fn style(&self, role: Role) -> Style {
        Style::new().fg_color(self.color(role))
    }
}

static CURRENT: OnceLock<Theme> = OnceLock::new();

/// Install the run's theme, once, at startup.
pub fn install(theme: Theme) {
    let _ = CURRENT.set(theme);
}

/// The run's theme; plain until [`install`] (so unit tests are plain).
pub fn current() -> &'static Theme {
    static PLAIN: OnceLock<Theme> = OnceLock::new();
    CURRENT
        .get()
        .unwrap_or_else(|| PLAIN.get_or_init(Theme::plain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_follow_sb_go_names() {
        assert_eq!(Profile::parse(None), (Profile::TrueColor, None));
        // sb-go reads an empty value as unset.
        assert_eq!(Profile::parse(Some("")), (Profile::TrueColor, None));
        assert_eq!(
            Profile::parse(Some("truecolor")),
            (Profile::TrueColor, None)
        );
        assert_eq!(Profile::parse(Some("ansi256")), (Profile::Ansi256, None));
        assert_eq!(Profile::parse(Some("ansi")), (Profile::Ansi, None));
        assert_eq!(Profile::parse(Some("ascii")), (Profile::Ascii, None));
    }

    /// An unknown value means truecolor, as in sb-go, and (unlike sb-go,
    /// which is silent) warns.
    #[test]
    fn an_unknown_profile_warns_and_means_truecolor() {
        let (profile, warning) = Profile::parse(Some("16bit"));
        assert_eq!(profile, Profile::TrueColor);
        assert_eq!(
            warning.as_deref(),
            Some(
                "unknown SB_COLOR_PROFILE \"16bit\" (truecolor, ansi256, ansi, ascii); using truecolor"
            )
        );
    }

    #[test]
    fn each_profile_resolves_the_palette_its_own_way() {
        let rgb = anstyle::RgbColor(0xFF, 0x6E, 0x7A);
        assert_eq!(
            Profile::TrueColor.resolve(rgb),
            Some(anstyle::Color::Rgb(rgb))
        );
        assert_eq!(
            Profile::Ansi256.resolve(rgb),
            Some(anstyle::Color::Ansi256(anstyle_lossy::rgb_to_xterm(rgb)))
        );
        assert_eq!(
            Profile::Ansi.resolve(rgb),
            Some(anstyle::Color::Ansi(anstyle_lossy::rgb_to_ansi(
                rgb,
                anstyle_lossy::palette::VGA
            )))
        );
        assert_eq!(Profile::Ascii.resolve(rgb), None);
    }

    /// The built-in palette is the role-named one the prototypes were
    /// reviewed with (proto-theme).
    #[test]
    fn the_built_in_palette_keeps_the_reviewed_values() {
        let palette = Palette::default();
        assert_eq!(
            palette.get(Role::Problem),
            anstyle::RgbColor(0xFF, 0x6E, 0x7A)
        );
        assert_eq!(palette.get(Role::Ok), anstyle::RgbColor(0x6F, 0xD3, 0x9A));
        assert_eq!(
            palette.get(Role::Muted),
            anstyle::RgbColor(0x8B, 0x95, 0xA7)
        );
    }

    /// Every role against the reviewed proto-theme value, by name, so two
    /// swapped table entries can't pass.
    #[test]
    fn every_role_keeps_its_reviewed_value() {
        let reviewed: [(Role, u32); Role::ALL.len()] = [
            (Role::Text, 0xDCE3EC),
            (Role::Muted, 0x8B95A7),
            (Role::Faint, 0x586174),
            (Role::Border, 0x3A4558),
            (Role::Accent, 0x5CCFE6),
            (Role::OnAccent, 0x0B1A24),
            (Role::Ok, 0x6FD39A),
            (Role::Problem, 0xFF6E7A),
            (Role::Warn, 0xF5B55B),
            (Role::Idle, 0x84A9C0),
            (Role::Service, 0x7EA6FF),
            (Role::ServiceRule, 0x34466E),
            (Role::Container, 0xC49BFF),
            (Role::ContainerRule, 0x4F3D6E),
            (Role::AttentionRule, 0x6A2E36),
            (Role::Highlight, 0xE8C77A),
            (Role::Link, 0x7EA6FF),
            (Role::Code, 0xE8C77A),
            (Role::CodeBg, 0x1C2430),
            (Role::Accent2, 0xC49BFF),
            (Role::ButtonBg, 0x2A3446),
            (Role::ButtonDisabledBg, 0x1A2029),
            (Role::SearchHit, 0x4A3F12),
            (Role::SelectedFg, 0xFFFFFF),
            (Role::SelectedBg, 0x24405E),
        ];
        let palette = Palette::default();
        for (role, hex) in reviewed {
            let [_, r, g, b] = hex.to_be_bytes();
            assert_eq!(
                palette.get(role),
                anstyle::RgbColor(r, g, b),
                "{}",
                role.name()
            );
        }
    }

    /// The table is indexed by role, so the two orders must agree.
    #[test]
    fn roles_index_their_own_place() {
        assert!(
            Role::ALL
                .iter()
                .enumerate()
                .all(|(i, role)| role.index() == i)
        );
    }

    /// A pinned 16-colour result, so an anstyle-lossy upgrade that changes
    /// the mapping is noticed: the problem red is bright red (SGR 91).
    #[test]
    fn problem_red_is_bright_red_in_sixteen_colours() {
        assert_eq!(
            Profile::Ansi.resolve(anstyle::RgbColor(0xFF, 0x6E, 0x7A)),
            Some(anstyle::Color::Ansi(anstyle::AnsiColor::BrightRed))
        );
    }

    /// Every role has a unique config name (the theme file uses them).
    #[test]
    fn role_names_are_unique() {
        let mut names: Vec<_> = Role::ALL.iter().map(|r| r.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Role::ALL.len());
    }

    /// Background roles resolve too (`color`, not only foreground `style`).
    #[test]
    fn colour_gives_any_role_including_backgrounds() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        assert_eq!(
            theme.color(Role::SelectedBg),
            Some(anstyle::Color::Rgb(anstyle::RgbColor(0x24, 0x40, 0x5E)))
        );
        assert_eq!(Theme::plain().color(Role::SelectedBg), None);
    }

    #[test]
    fn a_plain_theme_has_no_colour() {
        assert_eq!(Theme::plain().style(Role::Problem), anstyle::Style::new());
    }
}
