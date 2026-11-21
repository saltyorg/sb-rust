//! Section headings (term-ui spec, "Tables", section headings): `sb
//! list`'s heading over each repository, shared with `sb bench`'s
//! sections. A name, bold, in its role; hints after it, each in its own
//! role; then a rule across the width. anstyle's own pattern
//! (<https://docs.rs/anstyle/1.0.14/anstyle/struct.Style.html>), as
//! [`crate::style::paint`]: a style rendered before the text and its
//! reset after it.

use std::fmt::Write as _;

use sb_cli_core::theme::{Profile, Role, Theme};

/// A section heading: its name, the hints after it and the rule under
/// it, each in a theme role. Its text is sb's own words, never untrusted
/// input, so it is written as given.
#[derive(Clone, Debug)]
pub struct Heading {
    name: String,
    role: Role,
    hints: Vec<(String, Role)>,
    rule: Role,
}

impl Heading {
    /// A heading of `name`, bold in `role`, its rule in Muted.
    pub fn new(name: impl Into<String>, role: Role) -> Self {
        Heading {
            name: name.into(),
            role,
            hints: Vec::new(),
            rule: Role::Muted,
        }
    }

    /// Add `text` after the name (and any earlier hint), in `role`: its
    /// spacing is the caller's (`"  31 tags"`).
    #[must_use]
    pub fn hint(mut self, text: impl Into<String>, role: Role) -> Self {
        self.hints.push((text.into(), role));
        self
    }

    /// Draw the rule in `role` instead of Muted.
    #[must_use]
    pub fn rule(mut self, role: Role) -> Self {
        self.rule = role;
        self
    }

    /// The heading's two lines, each ending in `\n`: the name and hints,
    /// then a rule `width` wide. In `theme`'s roles, the name bold, when
    /// `colour` is on and the profile isn't `ascii`; otherwise no codes
    /// at all, bold included, and the rule in `-`.
    #[must_use]
    pub fn render(&self, theme: &Theme, colour: bool, width: usize) -> String {
        let styled = colour && theme.profile() != Profile::Ascii;
        let plain = Theme::plain();
        let theme = if styled { theme } else { &plain };
        let name = theme.style(self.role);
        let name = if styled { name.bold() } else { name };
        let mut out = format!("{name}{}{name:#}", self.name);
        for (text, role) in &self.hints {
            let style = theme.style(*role);
            let _ = write!(out, "{style}{text}{style:#}");
        }
        let _ = writeln!(out, "\n{}", rule(theme, self.rule, styled, width));
        out
    }
}

/// A rule `width` wide in `role` (a heading's, or `sb bench`'s footer's),
/// without a line end: `─` in `theme`'s colour when `colour` is on and
/// the profile isn't `ascii`, else `-` with no codes. Its arguments in
/// [`crate::style::paint`]'s order.
#[must_use]
pub fn rule(theme: &Theme, role: Role, colour: bool, width: usize) -> String {
    if colour && theme.profile() != Profile::Ascii {
        let style = theme.style(role);
        format!("{style}{}{style:#}", "─".repeat(width))
    } else {
        "-".repeat(width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_cli_core::theme::Palette;

    fn theme(profile: Profile) -> Theme {
        Theme::resolve(&Palette::default(), profile)
    }

    fn listed() -> Heading {
        Heading::new("Sandbox", Role::Service)
            .hint("  8 tags", Role::Muted)
            .hint("  ·  install as ", Role::Muted)
            .hint("sandbox-<tag>", Role::Accent)
    }

    /// In colour: the name bold in its role, each hint in its own, the
    /// rule a `─` across the width in Muted.
    #[test]
    fn a_heading_draws_its_roles_and_a_rule_across_the_width() {
        let theme = theme(Profile::TrueColor);
        let text = listed().render(&theme, true, 12);
        let name = theme.style(Role::Service).bold();
        let (muted, accent) = (theme.style(Role::Muted), theme.style(Role::Accent));
        assert_eq!(
            text,
            format!(
                "{name}Sandbox{name:#}{muted}  8 tags{muted:#}{muted}  ·  install as {muted:#}\
                 {accent}sandbox-<tag>{accent:#}\n{muted}{}{muted:#}\n",
                "─".repeat(12)
            )
        );
        assert!(text.starts_with("\x1b[1m\x1b[38;2;"), "{text:?}");
    }

    /// The rule takes the role its caller names.
    #[test]
    fn the_rule_takes_the_role_it_is_given() {
        let theme = theme(Profile::TrueColor);
        let text = Heading::new("System", Role::Service)
            .rule(Role::Border)
            .render(&theme, true, 3);
        let border = theme.style(Role::Border);
        assert!(
            text.ends_with(&format!("\n{border}───{border:#}\n")),
            "{text:?}"
        );
        assert_ne!(border, theme.style(Role::Muted));
    }

    /// Colour off, or `ascii`: no codes at all, bold included, the rule in
    /// `-`; a heading with no hints is its name alone.
    #[test]
    fn a_plain_heading_has_no_codes_and_a_dashed_rule() {
        for (profile, colour) in [(Profile::TrueColor, false), (Profile::Ascii, true)] {
            let text = listed().render(&theme(profile), colour, 5);
            assert_eq!(
                text, "Sandbox  8 tags  ·  install as sandbox-<tag>\n-----\n",
                "{profile:?}"
            );
        }
        assert_eq!(
            Heading::new("System", Role::Service).render(&theme(Profile::Ascii), true, 2),
            "System\n--\n"
        );
    }

    /// A rule alone: in its role with colour, `-` without or under
    /// `ascii`.
    #[test]
    fn a_rule_alone_is_one_line_in_its_role() {
        let colour = theme(Profile::TrueColor);
        let muted = colour.style(Role::Muted);
        assert_eq!(
            rule(&colour, Role::Muted, true, 4),
            format!("{muted}────{muted:#}")
        );
        assert_eq!(rule(&colour, Role::Muted, false, 4), "----");
        assert_eq!(rule(&theme(Profile::Ascii), Role::Muted, true, 2), "--");
    }
}
