//! What `sb update` moved, and its closing hint (ops spec, "What moved,
//! and the closing hint"; decisions 36 to 38): after the last step tree,
//! each checkout whose pull finished, its commit range or `already up to
//! date`, the tags the pull added or removed when they were compared,
//! then, after a finished run where a checkout moved, how to apply it.

use std::collections::HashSet;
use std::fmt::Write as _;

use sb_cli_core::exit::escaped;
use sb_cli_core::theme::{Role, Theme};
use sb_term_ui::wrap_line;

use crate::Checkout;

/// The block's width: 80 whatever the terminal's (spec: one layout to
/// read and paste).
const WIDTH: usize = 80;

/// The column the tags start at: `  Removed tags:  `'s width.
const LABEL: usize = 17;

/// The lines a list takes at most before ` and <n> more (sb list)`.
const MOST_LINES: usize = 2;

/// The width a hint's command is padded to: its description starts at
/// column 29, after the two-space indent.
const HINT_COMMAND: usize = 27;

/// The width a checkout's name is padded to: `Saltbox  `.
const NAME: usize = 9;

/// One checkout whose pull finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Moved {
    pub checkout: Checkout,
    /// HEAD before the fetch and after it (full ids).
    pub old: String,
    pub new: String,
    /// The tags the pull added and removed, when the old cache entry was
    /// valid for `old` and the rebuild finished; else `None`.
    pub tags: Option<TagDiff>,
}

/// The tags a pull added and removed, as the user types them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TagDiff {
    /// In the new listing's order.
    pub added: Vec<String>,
    /// In the old listing's order.
    pub removed: Vec<String>,
}

impl TagDiff {
    /// `before` against `after` (each in Ansible's order), the tags
    /// named as `sb install` takes them: Saltbox's as listed, Sandbox's
    /// with `sandbox-`.
    pub(crate) fn between(checkout: Checkout, before: &[String], after: &[String]) -> Self {
        let prefix = match checkout {
            Checkout::Saltbox => "",
            Checkout::Sandbox => "sandbox-",
        };
        let only = |these: &[String], those: &[String]| {
            let those: HashSet<&String> = those.iter().collect();
            these
                .iter()
                .filter(|tag| !those.contains(tag))
                .map(|tag| format!("{prefix}{tag}"))
                .collect()
        };
        Self {
            added: only(after, before),
            removed: only(before, after),
        }
    }

    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// The block for `moved` (Saltbox's first, as update pulls them), in
/// `theme`'s roles, and the hint when `finished` (the run ended well)
/// and a checkout moved: a blank line, the block, then a blank line and
/// the hint. Nothing when no pull finished.
pub(crate) fn render(moved: &[Moved], finished: bool, theme: &Theme) -> String {
    if moved.is_empty() {
        return String::new();
    }
    let muted = theme.style(Role::Muted);
    let mut out = String::from("\n");
    for one in moved {
        let name = one.checkout.name();
        let (old, new) = (short(&one.old), short(&one.new));
        if one.old == one.new {
            let _ = writeln!(
                out,
                "{name:<NAME$}already up to date ({muted}{new}{muted:#})"
            );
            continue;
        }
        let _ = write!(out, "{name:<NAME$}{muted}{old} → {new}{muted:#}");
        match &one.tags {
            Some(diff) if diff.is_empty() => out.push_str(", no tag changes\n"),
            Some(diff) => {
                out.push('\n');
                for (label, tags, role) in [
                    ("New tags:", &diff.added, Role::Ok),
                    ("Removed tags:", &diff.removed, Role::Warn),
                ] {
                    if !tags.is_empty() {
                        out.push_str(&list(label, tags, role, theme));
                    }
                }
            }
            None => out.push('\n'),
        }
    }
    if finished {
        out.push_str(&hint(moved, theme));
    }
    out
}

/// A commit's id as the block shows it: its first 7 characters.
fn short(id: &str) -> String {
    id.chars().take(7).collect()
}

/// `  <label>` then `tags` (escaped: Ansible's output) joined with `, `
/// in `role`, wrapped at words to [`WIDTH`] under the label (term-ui's
/// `wrap_line`, the tree's), and capped at [`MOST_LINES`]: when the whole
/// list needs more, as many tags as fit, then ` and <n> more (sb list)`.
/// Why not tabled's `Truncate::suffix`
/// (<https://docs.rs/tabled/0.22.0/tabled/settings/width/struct.Truncate.html#method.suffix>):
/// it cuts at a width, in the middle of a tag, with a fixed suffix, where
/// the cap keeps whole tags and counts the rest; no dependency fits a
/// list by its items. The search wraps the list again for each tag it
/// drops: a few hundred tags at most, once per run.
fn list(label: &str, tags: &[String], role: Role, theme: &Theme) -> String {
    let (style, code) = (theme.style(role), theme.style(Role::Code));
    let shown: Vec<String> = tags.iter().map(|tag| escaped(tag)).collect();
    let lines = |count: usize| {
        let mut text = format!("{style}{}{style:#}", shown[..count].join(", "));
        if count < shown.len() {
            let more = shown.len() - count;
            let _ = write!(text, " and {more} more ({code}sb list{code:#})");
        }
        wrap_line(&text, WIDTH - LABEL, true)
    };
    let mut count = shown.len();
    let mut wrapped = lines(count);
    while wrapped.len() > MOST_LINES && count > 1 {
        count -= 1;
        wrapped = lines(count);
    }
    let mut out = String::new();
    for (at, line) in wrapped.iter().enumerate() {
        let lead = if at == 0 {
            format!("  {label}")
        } else {
            String::new()
        };
        let _ = writeln!(out, "{lead:<LABEL$}{line}");
    }
    out
}

/// The closing hint after a blank line: Saltbox's (hint H2) when Saltbox
/// moved, Sandbox's when only Sandbox did, else nothing; the commands
/// in Code.
fn hint(moved: &[Moved], theme: &Theme) -> String {
    let moved_one = |checkout| {
        moved
            .iter()
            .any(|one| one.checkout == checkout && one.old != one.new)
    };
    let (heading, commands): (&str, [(&str, &str); 2]) = if moved_one(Checkout::Saltbox) {
        (
            "Saltbox's files are updated; your server and apps are not yet. To apply it:",
            [
                (
                    "sb install saltbox",
                    "your main tag (or mediabox, feederbox); includes core",
                ),
                (
                    "sb install core,<apps>",
                    "the minimum: core plus the apps you run",
                ),
            ],
        )
    } else if moved_one(Checkout::Sandbox) {
        (
            "Sandbox's files are updated; your Sandbox apps are not yet. To apply it:",
            [
                ("sb install sandbox-<app>", "each Sandbox app you run"),
                (
                    "sb install sandbox-roles",
                    "the apps you list in sandbox_roles",
                ),
            ],
        )
    } else {
        return String::new();
    };
    let code = theme.style(Role::Code);
    let mut out = format!("\n{heading}\n");
    for (command, what) in commands {
        let pad = HINT_COMMAND.saturating_sub(command.len());
        let _ = writeln!(out, "  {code}{command}{code:#}{:pad$}{what}", "");
    }
    out
}

#[cfg(test)]
mod tests {
    use sb_cli_core::theme::{Palette, Profile};

    use super::*;

    fn tags(names: &[&str]) -> Vec<String> {
        names.iter().map(ToString::to_string).collect()
    }

    fn moved(checkout: Checkout, old: &str, new: &str, tags: Option<TagDiff>) -> Moved {
        Moved {
            checkout,
            old: old.repeat(40),
            new: new.repeat(40),
            tags,
        }
    }

    fn diff(added: &[&str], removed: &[&str]) -> TagDiff {
        TagDiff {
            added: tags(added),
            removed: tags(removed),
        }
    }

    /// The tags each side lacks, each in its own listing's order,
    /// Sandbox's with `sandbox-`.
    #[test]
    fn a_diff_is_the_tags_each_listing_lacks_as_typed() {
        let before = tags(&["always", "hoarder", "plex", "sonarr"]);
        let after = tags(&["tracearr", "always", "karakeep", "plex"]);
        assert_eq!(
            TagDiff::between(Checkout::Sandbox, &before, &after),
            diff(
                &["sandbox-tracearr", "sandbox-karakeep"],
                &["sandbox-hoarder", "sandbox-sonarr"]
            )
        );
        assert_eq!(
            TagDiff::between(Checkout::Saltbox, &before, &after),
            diff(&["tracearr", "karakeep"], &["hoarder", "sonarr"])
        );
        assert!(TagDiff::between(Checkout::Saltbox, &before, &before).is_empty());
    }

    /// Each line kind of the plain block (the spec's examples), Saltbox
    /// first, and the hint only for a finished run.
    #[test]
    fn each_line_reads_as_the_spec_shows_it() {
        let plain = Theme::plain();
        assert_eq!(render(&[], true, &plain), "");
        let block = [
            moved(Checkout::Saltbox, "a", "e", None),
            moved(
                Checkout::Sandbox,
                "0",
                "c",
                Some(diff(
                    &["sandbox-karakeep", "sandbox-tracearr"],
                    &["sandbox-hoarder"],
                )),
            ),
        ];
        assert_eq!(
            render(&block, false, &plain),
            "\nSaltbox  aaaaaaa → eeeeeee\nSandbox  0000000 → ccccccc\n  \
             New tags:      sandbox-karakeep, sandbox-tracearr\n  \
             Removed tags:  sandbox-hoarder\n"
        );
        let block = [
            moved(Checkout::Saltbox, "e", "e", None),
            moved(Checkout::Sandbox, "c", "d", Some(TagDiff::default())),
        ];
        assert_eq!(
            render(&block, false, &plain),
            "\nSaltbox  already up to date (eeeeeee)\nSandbox  ccccccc → ddddddd, no tag changes\n"
        );
        // Only removed tags: no New tags line.
        let block = [moved(Checkout::Saltbox, "a", "b", Some(diff(&[], &["x"])))];
        assert_eq!(
            render(&block, false, &plain),
            "\nSaltbox  aaaaaaa → bbbbbbb\n  Removed tags:  x\n"
        );
    }

    /// H2 when Saltbox moved, with or without Sandbox; Sandbox's own when
    /// only Sandbox did; none when nothing moved or the run failed.
    #[test]
    fn the_hint_follows_what_moved() {
        let plain = Theme::plain();
        let h2 = "\nSaltbox's files are updated; your server and apps are not yet. To apply it:\n  \
                  sb install saltbox         your main tag (or mediabox, feederbox); includes core\n  \
                  sb install core,<apps>     the minimum: core plus the apps you run\n";
        let sandbox = "\nSandbox's files are updated; your Sandbox apps are not yet. To apply it:\n  \
                       sb install sandbox-<app>   each Sandbox app you run\n  \
                       sb install sandbox-roles   the apps you list in sandbox_roles\n";
        let saltbox_moved = moved(Checkout::Saltbox, "a", "b", None);
        let saltbox_still = moved(Checkout::Saltbox, "a", "a", None);
        let sandbox_moved = moved(Checkout::Sandbox, "c", "d", None);
        let sandbox_still = moved(Checkout::Sandbox, "c", "c", None);
        for block in [
            [saltbox_moved.clone(), sandbox_moved.clone()],
            [saltbox_moved.clone(), sandbox_still.clone()],
        ] {
            assert!(render(&block, true, &plain).ends_with(h2));
            assert!(!render(&block, false, &plain).contains("To apply"));
        }
        let only_sandbox = [saltbox_still.clone(), sandbox_moved];
        assert!(render(&only_sandbox, true, &plain).ends_with(sandbox));
        let still = [saltbox_still, sandbox_still];
        assert!(!render(&still, true, &plain).contains("To apply"));
        // Each description starts at the same column.
        for line in [h2, sandbox].iter().flat_map(|hint| hint.lines()) {
            if line.starts_with("  sb install") {
                assert_eq!(&line[28..29], " ", "{line:?}");
                assert_ne!(&line[29..30], " ", "{line:?}");
            }
        }
    }

    /// A list wraps at words under its label, every line within 80
    /// columns; one that needs more than two lines keeps as many tags as
    /// fit two, then `and <n> more (sb list)`.
    #[test]
    fn a_long_list_wraps_under_its_label_and_stops_at_two_lines() {
        let plain = Theme::plain();
        let names: Vec<String> = (b'a'..=b'q')
            .map(|letter| format!("sandbox-{}", char::from(letter)))
            .collect();
        let text = list("New tags:", &names, Role::Ok, &plain);
        assert_eq!(
            text,
            "  New tags:      sandbox-a, sandbox-b, sandbox-c, sandbox-d, sandbox-e,\n                 \
             sandbox-f, sandbox-g, sandbox-h, sandbox-i and 8 more (sb list)\n"
        );
        for line in text.lines() {
            assert!(line.chars().count() <= WIDTH, "{line:?}");
        }
        // Ten fit two lines whole: no `more`.
        let text = list("New tags:", &names[..10], Role::Ok, &plain);
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(!text.contains("more"), "{text}");
        // One that fits one line.
        assert_eq!(
            list("Removed tags:", &names[..1], Role::Warn, &plain),
            "  Removed tags:  sandbox-a\n"
        );
        // A tag wider than the room (none is, in practice) breaks
        // anywhere: the first tag stays, every line within 80 columns.
        let wide = ["x".repeat(200), "y".to_owned()];
        let text = list("New tags:", &wide, Role::Ok, &plain);
        assert!(text.ends_with(" and 1 more (sb list)\n"), "{text}");
        // One such tag alone is shown whole, over as many lines as it needs.
        let text = list("New tags:", &wide[..1], Role::Ok, &plain);
        assert_eq!(text.lines().count(), 4, "{text}");
        for line in text.lines() {
            assert!(line.chars().count() <= WIDTH, "{line:?}");
        }
        assert!(
            text.starts_with(&format!("  New tags:      {}\n", "x".repeat(63))),
            "{text}"
        );
    }

    /// Escaped tags: a control character in Ansible's output can't reach
    /// the terminal.
    #[test]
    fn a_tag_is_escaped() {
        let text = list(
            "New tags:",
            &tags(&["pl\u{1b}[2Jex"]),
            Role::Ok,
            &Theme::plain(),
        );
        assert!(!text.contains('\u{1b}'), "{text:?}");
    }

    /// The roles: ids and arrow Muted, new tags Ok, removed Warn, `sb
    /// list` and the hint's commands Code.
    #[test]
    fn the_block_takes_the_themes_roles() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let style = |role| theme.style(role);
        let (muted, ok, warn, code) = (
            style(Role::Muted),
            style(Role::Ok),
            style(Role::Warn),
            style(Role::Code),
        );
        let block = [
            moved(Checkout::Saltbox, "e", "e", None),
            moved(
                Checkout::Sandbox,
                "a",
                "b",
                Some(diff(&["sandbox-x"], &["sandbox-y"])),
            ),
        ];
        let text = render(&block, true, &theme);
        assert!(
            text.contains(&format!("already up to date ({muted}eeeeeee{muted:#})")),
            "{text:?}"
        );
        assert!(
            text.contains(&format!("Sandbox  {muted}aaaaaaa → bbbbbbb{muted:#}\n")),
            "{text:?}"
        );
        assert!(text.contains(&format!("{ok}sandbox-x{ok:#}")), "{text:?}");
        assert!(
            text.contains(&format!("{warn}sandbox-y{warn:#}")),
            "{text:?}"
        );
        assert!(
            text.contains(&format!(
                "  {code}sb install sandbox-roles{code:#}   the apps"
            )),
            "{text:?}"
        );
        let names: Vec<String> = (0..40).map(|at| format!("tag-{at}")).collect();
        let text = list("New tags:", &names, Role::Ok, &theme);
        // Wrapped by tabled, which closes a colour with its own reset.
        assert!(
            text.contains(&format!("({}sb list", code.render())),
            "{text:?}"
        );
    }
}
