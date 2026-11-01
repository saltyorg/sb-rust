//! proto-help: sb's `--help` drawn by the Rust options there are, next to
//! sb-go's (fang) help. See FINDINGS.md.
//!
//! `proto-help` prints the gallery; `proto-help <look> [command...]` prints
//! one look for one command (`proto-help sb-go install`). Looks: clap,
//! palette, cargo, template, clap-help, sb-go. The command tree is sb-go
//! 0.1.3's (`commands.tsv`).

use std::collections::BTreeMap;
use std::io::Write;

use anstyle::Style;
use clap::builder::{StyledStr, Styles};
use clap::{Arg, ArgAction, Command};
use sb_cli_core::theme::{Palette, Profile, Role, Theme};

const TREE: &str = include_str!("commands.tsv");

/// Ids for positional arguments (clap ids are `'static`).
const ARG_IDS: [&str; 4] = ["arg1", "arg2", "arg3", "arg4"];

#[derive(Clone, Copy, PartialEq)]
enum Look {
    /// clap's defaults: what sb shows today.
    Clap,
    /// clap's `Styles` filled from sb's palette, nothing else
    /// (https://docs.rs/clap/4.6.7/clap/builder/struct.Styles.html).
    Palette,
    /// cargo's own look: clap-cargo's `CLAP_STYLING` preset
    /// (https://docs.rs/clap-cargo/0.19.0/clap_cargo/style/) and a help
    /// template with color-print markup, as cargo itself does
    /// (rust-lang/cargo#12578).
    Cargo,
    /// clap's help template with sb-go's sections as palette badges
    /// (https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.help_template).
    Template,
    /// clap-help: termimad tables (https://docs.rs/clap-help/1.7.0/). No
    /// subcommand support (its README).
    ClapHelp,
    /// sb-go's fang layout, drawn by our own code from clap's command data,
    /// in sb's palette.
    SbGo,
}

const LOOKS: [(Look, &str, &str); 6] = [
    (
        Look::Clap,
        "clap",
        "1. clap's defaults (sb today, on sb-go's whole tree)",
    ),
    (
        Look::Palette,
        "palette",
        "2. clap, Styles from sb's palette",
    ),
    (
        Look::Cargo,
        "cargo",
        "3. cargo's look: clap-cargo styles + color-print template",
    ),
    (
        Look::Template,
        "template",
        "4. clap template: sb-go's sections, palette badges",
    ),
    (
        Look::ClapHelp,
        "clap-help",
        "5. clap-help (termimad tables; no subcommands)",
    ),
    (
        Look::SbGo,
        "sb-go",
        "6. sb-go's format copied (own layout, sb's palette)",
    ),
];

struct Flag {
    scope: &'static str,
    name: &'static str,
    short: &'static str,
    switch: bool,
    hidden: bool,
    usage: &'static str,
    default: &'static str,
}

#[derive(Default)]
struct Node {
    hidden: bool,
    usage: &'static str,
    about: &'static str,
    /// cobra's `Long`, newlines unescaped.
    long: String,
    flags: Vec<Flag>,
}

type Tree = BTreeMap<&'static str, Node>;

fn tree() -> Tree {
    let mut nodes = Tree::new();
    for line in TREE.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "command" => {
                nodes.insert(
                    f[1],
                    Node {
                        hidden: f[2] == "true",
                        usage: f[3],
                        about: f[4],
                        long: f[5].replace("\\n", "\n").replace("\\\\", "\\"),
                        flags: Vec::new(),
                    },
                );
            }
            _ => nodes.entry(f[1]).or_default().flags.push(Flag {
                scope: f[2],
                name: f[3],
                short: f[4],
                switch: f[5] == "switch",
                hidden: f[6] == "true",
                usage: f[7],
                default: f[8],
            }),
        }
    }
    nodes
}

fn children<'a>(nodes: &'a Tree, path: &str) -> Vec<(&'static str, &'a Node)> {
    nodes
        .iter()
        .filter(|(p, _)| {
            p.strip_prefix(path)
                .and_then(|rest| rest.strip_prefix(' '))
                .is_some_and(|rest| !rest.contains(' '))
        })
        .map(|(p, n)| (*p, n))
        .collect()
}

fn palette_styles(theme: &Theme) -> Styles {
    Styles::styled()
        .header(theme.style(Role::Accent).bold())
        .usage(theme.style(Role::Accent).bold())
        .literal(theme.style(Role::Highlight).bold())
        .placeholder(theme.style(Role::Muted))
        .valid(theme.style(Role::Ok))
        .invalid(theme.style(Role::Warn))
        .error(theme.style(Role::Problem).bold())
}

fn badge(theme: &Theme, text: &str) -> String {
    let style = Style::new()
        .fg_color(theme.color(Role::OnAccent))
        .bg_color(theme.color(Role::Accent))
        .bold();
    format!("  {style} {text} {style:#}")
}

fn template(look: Look, theme: &Theme, has_children: bool) -> Option<String> {
    let commands = |heading: &str| {
        if has_children {
            format!("{heading}\n{{subcommands}}\n\n")
        } else {
            String::new()
        }
    };
    match look {
        Look::Cargo => Some(format!(
            "{{about-with-newline}}\n{}{{usage}}\n\n{}{}\n{{options}}\n",
            color_print::cstr!("<green,bold>Usage:</> "),
            commands(color_print::cstr!("<green,bold>Commands:</>")),
            color_print::cstr!("<green,bold>Options:</>"),
        )),
        Look::Template => Some(format!(
            "\n  {{about}}\n\n{}\n\n    {{usage}}\n\n{}{}\n\n{{options}}\n",
            badge(theme, "USAGE"),
            commands(&format!("{}\n", badge(theme, "COMMANDS"))),
            badge(theme, "FLAGS")
        )),
        _ => None,
    }
}

fn build(look: Look, theme: &Theme, nodes: &Tree, path: &'static str) -> Command {
    let node = &nodes[path];
    let name = path.rsplit(' ').next().unwrap_or(path);
    let mut cmd = Command::new(name);
    if path == "sb" {
        // sb's real `--color`: colorchoice-clap's flag, as sb flattens it
        // (https://docs.rs/clap/4.6.7/clap/trait.Args.html#tymethod.augment_args).
        // First: its `#[command(about = None)]` would clear the about.
        cmd = <sb_cli_core::colour::Color as clap::Args>::augment_args(cmd);
    }
    let mut cmd = cmd.about(node.about).hide(node.hidden);
    if !node.long.is_empty() {
        cmd = cmd.long_about(StyledStr::from(node.long.clone()));
    }
    for f in &node.flags {
        let mut arg = Arg::new(f.name)
            .long(f.name)
            .help(f.usage)
            .hide(f.hidden)
            .global(f.scope == "global")
            .action(if f.switch {
                ArgAction::SetTrue
            } else {
                ArgAction::Set
            });
        if let Some(c) = f.short.chars().next() {
            arg = arg.short(c);
        }
        if !f.switch {
            // The placeholder a derive would give (`<EXTRA_VARS>`); leaked,
            // since clap takes `'static` names without its `string` feature.
            let upper: &'static str =
                Box::leak(f.name.replace('-', "_").to_uppercase().into_boxed_str());
            arg = arg.value_name(upper);
            if !f.default.is_empty() {
                arg = arg.default_value(f.default);
            }
        }
        cmd = cmd.arg(arg);
    }
    // Positionals from cobra's `Use` (`[tags]` optional, `<a|b>` required),
    // so clap's usage line and the sb-go hints come from clap's own data.
    for (i, token) in node.usage.split(' ').skip(1).enumerate() {
        let required = token.starts_with('<');
        let value = token.trim_matches(|c| matches!(c, '[' | ']' | '<' | '>'));
        cmd = cmd.arg(
            Arg::new(ARG_IDS[i])
                .value_name(value)
                .required(required)
                .index(i + 1),
        );
    }
    let kids = children(nodes, path);
    let has_children = !kids.is_empty();
    for (child, _) in kids {
        cmd = cmd.subcommand(build(look, theme, nodes, child));
    }
    if has_children {
        cmd = cmd.subcommand_required(true);
    }
    cmd = match look {
        Look::Palette | Look::Template => cmd.styles(palette_styles(theme)),
        Look::Cargo => cmd.styles(clap_cargo::style::CLAP_STYLING),
        _ => cmd,
    };
    // cargo sets its template on `cargo` only; subcommands keep clap's.
    match template(look, theme, has_children) {
        Some(_) if look == Look::Cargo && path != "sb" => cmd,
        Some(t) => cmd.help_template(StyledStr::from(t)),
        None => cmd,
    }
}

/// sb-go's fang layout drawn from clap's own command data (one source of
/// truth): heading, usage in a shaded block, commands with fang's hints
/// (`[command]`, then the positionals, then `[--flags]`, as fang v2.0.1
/// help.go), flags as `-s --long` sorted by name, all in one column.
/// UNVERIFIED: our own layout; no library draws fang's format, and clap's
/// command list shows names only (help_template.rs). Widths count chars
/// after stripping styles (anstream), fine for sb's ASCII names.
fn sb_go(theme: &Theme, cmd: &Command, path: &str, colour: bool) -> String {
    let visible = |c: &Command| {
        c.get_subcommands()
            .any(|s| !s.is_hide_set() && s.get_name() != "help")
    };
    // sb's `--color` is inherited by every command, so it counts towards
    // `[--flags]` only on the root; clap's own `--help` never does (as fang).
    let own_flags = |c: &Command| {
        c.get_arguments().any(|a| {
            !a.is_positional()
                && !a.is_hide_set()
                && a.get_id() != "help"
                && (a.get_id() != "color" || c.get_name() == "sb")
        })
    };
    let hint = |c: &Command| {
        let mut hint = String::new();
        if visible(c) {
            hint.push_str(" [command]");
        }
        for a in c.get_positionals() {
            for v in a.get_value_names().unwrap_or_default() {
                hint.push_str(&if a.is_required_set() {
                    format!(" <{v}>")
                } else {
                    format!(" [{v}]")
                });
            }
        }
        if own_flags(c) {
            hint.push_str(" [--flags]");
        }
        hint
    };
    let commands: Vec<(String, String, String)> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && c.get_name() != "help")
        .map(|c| {
            let about = c.get_about().map(ToString::to_string).unwrap_or_default();
            (c.get_name().to_string(), hint(c), about)
        })
        .collect();
    let mut flags: Vec<(String, String, String)> = cmd
        .get_arguments()
        .filter(|a| !a.is_positional() && !a.is_hide_set())
        .map(|a| {
            let long = a.get_long().unwrap_or_default().to_string();
            let shown = match a.get_short() {
                Some(c) => format!("-{c} --{long}"),
                None => format!("--{long}"),
            };
            let mut help = if a.get_id() == "help" {
                // fang's wording for cobra's help flag.
                format!("Help for {}", cmd.get_name())
            } else {
                a.get_help().map(ToString::to_string).unwrap_or_default()
            };
            // fang shows a default unless it is empty, false, 0 or []
            // (help.go).
            let default: Vec<String> = a
                .get_default_values()
                .iter()
                .map(|v| v.to_string_lossy().into_owned())
                .collect();
            let default = default.join(",");
            if !matches!(default.as_str(), "" | "false" | "0" | "[]") {
                help.push_str(&format!(" ({default})"));
            }
            (long, shown, help)
        })
        .collect();
    flags.sort();
    let len = |s: &str| anstream::adapter::strip_str(s).to_string().chars().count();
    let width = commands
        .iter()
        .map(|(n, h, _)| len(n) + len(h))
        .chain(flags.iter().map(|(_, f, _)| len(f)))
        .max()
        .unwrap_or(0)
        + 2;

    let text = theme.style(Role::Text);
    let heading = theme.style(Role::Accent).bold();
    let command = theme.style(Role::Highlight);
    let muted = theme.style(Role::Muted);
    let flag = theme.style(Role::Ok);
    let block = Style::new().bg_color(theme.color(Role::CodeBg));
    let prog = theme.style(Role::Link).bg_color(theme.color(Role::CodeBg));
    let args = muted.bg_color(theme.color(Role::CodeBg));
    // The program in the link colour, the command path in the command
    // colour, as fang does.
    let sub = command.bg_color(theme.color(Role::CodeBg));
    let (program, rest) = path.split_at(path.find(' ').unwrap_or(path.len()));

    // The usage line always ends in `[--flags]`: cobra's own `-h` counts for
    // the command shown (fang `c.HasFlags()`), unlike in the command list.
    let usage_hint = format!("{} [--flags]", hint(cmd).trim_end_matches(" [--flags]"));
    let block_width = len(path) + len(&usage_hint) + 4;
    // fang shows `Long`, else `Short` (help.go `cmp.Or`).
    let about = cmd
        .get_long_about()
        .or(cmd.get_about())
        .map(ToString::to_string)
        .unwrap_or_default();
    let about = about.replace('\n', "\n  ");
    let mut out = String::new();
    out.push_str(&format!("\n  {text}{about}{text:#}\n\n"));
    out.push_str(&format!("  {heading}USAGE{heading:#}\n\n"));
    // fang drops the padding rows without colour (help.go).
    let pad_line = if colour {
        format!("  {block}{}{block:#}\n", " ".repeat(block_width))
    } else {
        String::new()
    };
    out.push_str(&pad_line);
    out.push_str(&format!(
        "  {block}  {prog}{program}{prog:#}{sub}{rest}{sub:#}{args}{usage_hint}{args:#}{block}  {block:#}\n"
    ));
    out.push_str(&pad_line);
    if !commands.is_empty() {
        out.push_str(&format!("\n  {heading}COMMANDS{heading:#}\n\n"));
        for (n, h, about) in &commands {
            let pad = " ".repeat(width - len(n) - len(h));
            out.push_str(&format!(
                "    {command}{n}{command:#}{muted}{h}{muted:#}{pad}{text}{about}{text:#}\n"
            ));
        }
    }
    out.push_str(&format!("\n  {heading}FLAGS{heading:#}\n\n"));
    for (_, f, about) in &flags {
        let pad = " ".repeat(width - len(f));
        out.push_str(&format!(
            "    {flag}{f}{flag:#}{pad}{text}{about}{text:#}\n"
        ));
    }
    out
}

fn render<W: Write>(look: Look, path: &[String], out: &mut W, colour: bool) -> std::io::Result<()> {
    let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
    let nodes = tree();
    let full = std::iter::once("sb".to_string())
        .chain(path.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    let mut root = build(look, &theme, &nodes, "sb");
    root.build();
    if !matches!(look, Look::SbGo | Look::ClapHelp) {
        // Exactly what clap prints for `sb <path> --help`: parse it and
        // render the help "error"
        // (https://docs.rs/clap/4.6.7/clap/error/struct.Error.html#method.render),
        // so clap picks its short or long form as it would for a user.
        let argv = std::iter::once("sb".to_string())
            .chain(path.iter().cloned())
            .chain(std::iter::once("--help".to_string()));
        let help = match root.try_get_matches_from(argv) {
            Err(err) => err.render(),
            Ok(_) => StyledStr::from("(no help)"),
        };
        return write!(out, "{}", help.ansi());
    }
    let mut target = &mut root;
    for name in path {
        target = target
            .find_subcommand_mut(name)
            .unwrap_or_else(|| panic!("no command {name}"));
    }
    if look == Look::SbGo {
        return write!(out, "{}", sb_go(&theme, target, &full, colour));
    }
    // clap-help writes styled text itself (termimad), picking a light or
    // dark skin by asking the terminal (terminal-light).
    // As clap-help's README examples: the about as the introduction, no
    // author line.
    let about = target
        .get_about()
        .map(ToString::to_string)
        .unwrap_or_default();
    clap_help::Printer::new(target.clone())
        .with("introduction", Box::leak(about.into_boxed_str()))
        .without("author")
        .write_help(out)
}

/// One parse error's parts: clap's message, its tips and context lines,
/// and the failing command (from clap's usage), as cli-core's
/// `help::parse_error_block` extracts them.
struct ErrorParts {
    headline: String,
    lines: Vec<(Option<&'static str>, String)>,
    command: String,
}

fn error_parts(mut err: clap::Error) -> ErrorParts {
    use clap::error::{ContextKind, ContextValue};
    let command = match err.remove(ContextKind::Usage) {
        Some(ContextValue::StyledStr(usage)) => usage
            .to_string()
            .lines()
            .next()
            .unwrap_or_default()
            .trim_start_matches("Usage:")
            .split_whitespace()
            .take_while(|w| !w.starts_with(['[', '<', '-']))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    let command = if command.is_empty() {
        "sb".to_string()
    } else {
        command
    };
    let text = err.render().to_string();
    let mut it = text.lines();
    let first = it.next().unwrap_or_default();
    let headline = first.strip_prefix("error: ").unwrap_or(first).to_string();
    let mut lines = Vec::new();
    for line in it {
        let t = line.trim();
        if t.is_empty() {
            continue;
        } else if t.starts_with("For more information") {
            lines.push((Some("help:"), format!("try '{command} --help'")));
        } else if let Some(tip) = t.strip_prefix("tip: ") {
            lines.push((Some("tip:"), tip.to_string()));
        } else {
            lines.push((None, t.to_string()));
        }
    }
    ErrorParts {
        headline,
        lines,
        command,
    }
}

/// The parse errors the gallery shows, from sb's real tree.
const ERROR_CASES: [&[&str]; 3] = [
    &["sb", "verison"],
    &["sb", "install", "--bogus"],
    &["sb", "--color=bad", "version"],
];

const ERROR_LOOKS: [(&str, &str); 65] = [
    (
        "hint-ask",
        "H1. hint: did you mean 'version'?; [possible values: …] in green",
    ),
    (
        "hint-ask-use",
        "H2. as H1, values as hint: use auto, always or never",
    ),
    (
        "hint-column",
        "H3. hint as a column, no colons: hint  did you mean …?",
    ),
    ("hint-chevron", "H4. hint › did you mean 'version'?"),
    (
        "hint-ask-kind",
        "H5. as H1, naming it: did you mean the command 'version'?",
    ),
    (
        "hint-values-below",
        "H6. as H1, values on their own line under hint: possible values",
    ),
    (
        "round-label",
        "R1. orange tip: label, dim tip, wrong red, fix green",
    ),
    (
        "round-arrow",
        "R2. orange arrow, dim tip, wrong red, fix green",
    ),
    (
        "round-label-green",
        "R3. as R1, the label in green like the fix",
    ),
    (
        "round-arrow-green",
        "R4. as R2, the arrow in green like the fix",
    ),
    (
        "round-arrow-accent",
        "R5. as R2, the arrow in sb's accent colour",
    ),
    (
        "round-label-sbgo",
        "R6. as R1, with sb-go's pink ERROR badge",
    ),
    (
        "round-arrow-sbgo",
        "R7. as R2, with sb-go's pink ERROR badge",
    ),
    ("round-label-bold", "R8. as R1, label and fix in bold"),
    (
        "round-arrow-bold",
        "R9. as R2, arrow, wrong value and fix in bold",
    ),
    (
        "round-label-underline",
        "R10. as R1, the wrong value underlined",
    ),
    (
        "focus-wrong",
        "F1. only the wrong value highlighted; tip plain",
    ),
    (
        "focus-both",
        "F2. wrong value and the fix highlighted, nothing else",
    ),
    (
        "focus-arrow-fix",
        "F3. T5's arrow, plain; only the fix highlighted",
    ),
    (
        "focus-arrow-both",
        "F4. T5's arrow, plain; wrong value and fix highlighted",
    ),
    (
        "focus-bare",
        "F5. no label or arrow; wrong value and fix highlighted",
    ),
    (
        "focus-semantic",
        "F6. wrong value in the problem colour, fix in the OK colour",
    ),
    ("focus-arrow-semantic", "F7. T5's arrow with F6's colours"),
    ("focus-bold", "F8. wrong value and fix in bold, no colour"),
    ("focus-dim", "F9. T5's arrow, tip text dimmed, fix bright"),
    (
        "focus-underline",
        "F10. wrong value underlined (problem colour), fix highlighted",
    ),
    ("tip-tb-dark", "TB1. TIP badge: orange, dark text"),
    ("tip-tb-light", "TB2. TIP badge: orange, light text"),
    ("tip-tb-hint", "TB3. HINT badge instead of TIP"),
    ("tip-tb-values", "TB4. TIP badge, the tip's values in bold"),
    (
        "tip-eb-sbgo",
        "EB1. sb-go's ERROR badge (pink, cream), orange tip: label",
    ),
    (
        "tip-eb-light",
        "EB2. ERROR badge: problem colour, light text; orange tip: label",
    ),
    (
        "tip-eb-accent",
        "EB3. ERROR badge in help's accent colour; orange tip: label",
    ),
    (
        "tip-eb-kind",
        "EB4. ERROR badge names the kind; orange tip: label",
    ),
    ("tip-label", "T1. 11 with the tip: label in orange"),
    ("tip-line", "T2. 11 with the whole tip in orange"),
    ("tip-values", "T3. T1 plus quoted values in orange bold"),
    (
        "tip-values-cmd",
        "T4. bold orange label, values in help's command colour",
    ),
    ("tip-arrow", "T5. an orange arrow instead of the label"),
    ("tip-badge", "T6. an orange TIP badge, mirroring ERROR"),
    ("tip-bar", "T7. an orange bar beside the tip"),
    (
        "tip-cobra",
        "T8. orange \"Did you mean this?\", suggestion below; other tips as T3",
    ),
    (
        "cobra",
        "A. sb-go's content: tips as cobra's \"Did you mean this?\"",
    ),
    ("cobra-help", "B. as A, plus \"Run 'sb --help' for usage.\""),
    ("accent", "C. as A, badge in help's heading colour"),
    (
        "text",
        "D. as A, no background: bold ERROR in the problem colour",
    ),
    (
        "two-tone",
        "E. as A, two-segment badge: ERROR, then the kind",
    ),
    (
        "block",
        "F. as A, the message in a shaded block like help's usage",
    ),
    ("kind", "G. as A, the badge names the kind of error"),
    (
        "one-line",
        "H. as A, the suggestion on one line, highlighted",
    ),
    (
        "values",
        "I. as A, quoted values coloured like help (commands, flags)",
    ),
    ("values-cobra", "J. I and H together"),
    (
        "sbgo-exact",
        "8. sb-go exactly: cream on hot pink, message only",
    ),
    (
        "sbgo-palette",
        "9. sb-go's layout, badge in sb's problem colour, message only",
    ),
    ("sbgo-light", "10. as 9, light text on the badge"),
    ("sbgo-tip", "11. as 9, plus clap's tip"),
    (
        "sbgo-tip-help",
        "12. as 9, plus the tip and the help pointer",
    ),
    ("sbgo-red", "13. as 9, the message in the problem colour"),
    ("badge-below", "1. today: badge, message below"),
    ("badge-inline", "2. badge and message on one line"),
    ("heading", "3. ERROR as a heading, like help's sections"),
    ("cross", "4. sb's ✗ block (run errors' look)"),
    ("bar", "5. a problem-coloured bar beside the message"),
    ("rustc", "6. rustc/cargo style: error: with = notes"),
    (
        "usage",
        "7. badge, message, then the command's help in a code block",
    ),
];

fn draw_error(look: &str, p: &ErrorParts, theme: &Theme, colour: bool) -> String {
    let text = theme.style(Role::Text);
    let label = theme.style(Role::Muted);
    let problem = theme.style(Role::Problem);
    let bold_problem = problem.bold();
    let badge = Style::new()
        .bg_color(theme.color(Role::Problem))
        .fg_color(theme.color(Role::OnAccent))
        .bold();
    let body = |indent: &str| -> String {
        p.lines
            .iter()
            .map(|(name, line)| match name {
                Some(name) => format!("{indent}{label}{name}{label:#} {line}\n"),
                None => format!("{indent}{line}\n"),
            })
            .collect()
    };
    let headline = &p.headline;
    // sb-go's (fang's) frame: a padded badge between rows of spaces, then
    // the message, then a blank line; `extra` lines follow the message.
    let sbgo = |badge: Style, message: Style, extra: &[(Option<&str>, String)]| {
        let width = 10;
        let pad = " ".repeat(width);
        let mut out =
            format!("{pad}\n  {badge} ERROR {badge:#} \n{pad}\n  {message}{headline}{message:#}\n");
        if !extra.is_empty() {
            out.push('\n');
            for (name, line) in extra {
                match name {
                    Some(name) => out.push_str(&format!("  {label}{name}{label:#} {line}\n")),
                    None => out.push_str(&format!("  {line}\n")),
                }
            }
        }
        out.push('\n');
        out
    };
    let palette_badge = badge;
    let tips: Vec<(Option<&str>, String)> = p
        .lines
        .iter()
        .filter(|(n, _)| *n != Some("help:"))
        .cloned()
        .collect();
    // cobra's suggestion from clap's tip ("a similar subcommand exists:
    // 'version'" gives `version`), and the tips that aren't suggestions.
    let suggestion = p.lines.iter().find_map(|(n, line)| {
        (*n == Some("tip:"))
            .then(|| {
                line.split_once("exists: '")
                    .map(|(_, v)| v.trim_end_matches('\'').to_string())
            })
            .flatten()
    });
    let other_tips: Vec<(Option<&str>, String)> = p
        .lines
        .iter()
        .filter(|(n, line)| {
            *n != Some("help:") && !(*n == Some("tip:") && line.contains("exists: '"))
        })
        .cloned()
        .collect();
    let highlight = theme.style(Role::Highlight);
    let flag_style = theme.style(Role::Ok);
    let paint = |v: &str| {
        if v.starts_with('-') {
            format!("{flag_style}{v}{flag_style:#}")
        } else {
            format!("{highlight}{v}{highlight:#}")
        }
    };
    // Quoted values in clap's message, coloured as help colours them.
    let coloured_headline = || {
        let mut out = String::new();
        for (i, part) in headline.split('\'').enumerate() {
            if i % 2 == 1 {
                out.push('\'');
                out.push_str(&paint(part));
                out.push('\'');
            } else {
                out.push_str(part);
            }
        }
        out
    };
    let kind = || {
        let h = headline.as_str();
        if h.starts_with("unrecognized subcommand") {
            ("UNKNOWN COMMAND", "unknown command")
        } else if h.starts_with("unexpected argument") {
            ("UNKNOWN FLAG", "unknown flag")
        } else if h.starts_with("invalid value") {
            ("INVALID VALUE", "invalid value")
        } else {
            ("ERROR", "error")
        }
    };
    // sb-go's frame with a free badge string, message string and lines.
    let frame = |badge_line: String, message: String, lines: Vec<String>| {
        let pad = " ".repeat(10);
        let mut out = format!("{pad}\n  {badge_line}\n{pad}\n  {message}\n");
        if !lines.is_empty() {
            out.push('\n');
            for line in lines {
                out.push_str(&format!("  {line}\n"));
            }
        }
        out.push('\n');
        out
    };
    let did_you_mean = |one_line: bool, paint_it: bool| -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(s) = &suggestion {
            let shown = if paint_it { paint(s) } else { s.clone() };
            if one_line {
                lines.push(format!("Did you mean {shown}?"));
            } else {
                lines.push("Did you mean this?".to_string());
                lines.push(format!("    {shown}"));
            }
        }
        for (name, line) in &other_tips {
            match name {
                Some(name) => lines.push(format!("{label}{name}{label:#} {line}")),
                None => lines.push(line.clone()),
            }
        }
        lines
    };
    let badge_of = |bg: Option<anstyle::Color>, text: &str| {
        let style = Style::new()
            .bg_color(bg)
            .fg_color(theme.color(Role::OnAccent))
            .bold();
        format!("{style} {text} {style:#}")
    };
    let problem_bg = theme.color(Role::Problem);
    let warn = theme.style(Role::Warn);
    let warn_bold = warn.bold();
    // A tip's quoted values ('version', '-- --bogus') in `style`.
    let quoted = |text: &str, style: Style| {
        let mut out = String::new();
        for (i, part) in text.split('\'').enumerate() {
            if i % 2 == 1 {
                out.push_str(&format!("'{style}{part}{style:#}'"));
            } else {
                out.push_str(part);
            }
        }
        out
    };
    // Variant 11's frame (palette badge, message), with the tips drawn by
    // `tip`; context lines ([possible values: …]) stay plain.
    let tip_frame_with = |error_badge: String, tip: &dyn Fn(&str) -> Vec<String>| {
        let mut lines = Vec::new();
        for (name, line) in &p.lines {
            match name {
                Some("tip:") => lines.extend(tip(line)),
                Some("help:") => {}
                Some(name) => lines.push(format!("{label}{name}{label:#} {line}")),
                None => lines.push(line.clone()),
            }
        }
        frame(error_badge, headline.clone(), lines)
    };
    let tip_frame =
        |tip: &dyn Fn(&str) -> Vec<String>| tip_frame_with(badge_of(problem_bg, "ERROR"), tip);
    let tip_badge = |fg: Role, text: &str| {
        let b = Style::new()
            .bg_color(theme.color(Role::Warn))
            .fg_color(theme.color(fg))
            .bold();
        format!("{b} {text} {b:#}")
    };
    let orange_label = |t: &str| vec![format!("{warn}tip:{warn:#} {t}")];
    let styled_badge = |bg: Option<anstyle::Color>, fg: Option<anstyle::Color>, text: &str| {
        let b = Style::new().bg_color(bg).fg_color(fg).bold();
        format!("{b} {text} {b:#}")
    };
    // Only what matters gets colour: the wrong value (the message's first
    // quoted part) and the fix (the tip's last quoted part).
    let mark_first = |text: &str, style: Style| match text.split_once('\'') {
        Some((before, rest)) => match rest.split_once('\'') {
            Some((value, after)) => format!("{before}'{style}{value}{style:#}'{after}"),
            None => text.to_string(),
        },
        None => text.to_string(),
    };
    let mark_last = |text: &str, style: Style| match text.rsplit_once('\'') {
        Some((rest, after)) => match rest.rsplit_once('\'') {
            Some((before, value)) => format!("{before}'{style}{value}{style:#}'{after}"),
            None => text.to_string(),
        },
        None => text.to_string(),
    };
    let ok = theme.style(Role::Ok);
    let bold = Style::new().bold();
    let underline = problem.underline();
    let dim = theme.style(Role::Muted);
    // A focus layout: the message with its wrong value in `wrong`, each tip
    // as `prefix` + tip with its fix in `fix` (tip text in `tip_text`).
    let focus = |wrong: Option<Style>, prefix: &str, fix: Option<Style>, tip_text: Style| {
        let message = match wrong {
            Some(style) => mark_first(headline, style),
            None => headline.clone(),
        };
        let mut lines = Vec::new();
        for (name, line) in &p.lines {
            match name {
                Some("tip:") => {
                    let body = match fix {
                        Some(style) => {
                            let marked = mark_last(line, style);
                            // Re-open the tip's own style after the fix.
                            marked.replace(&format!("{style:#}"), &format!("{style:#}{tip_text}"))
                        }
                        None => line.clone(),
                    };
                    lines.push(format!("{prefix}{tip_text}{body}{tip_text:#}"));
                }
                Some("help:") => {}
                Some(name) => lines.push(format!("{name} {line}")),
                None => lines.push(line.clone()),
            }
        }
        frame(badge_of(problem_bg, "ERROR"), message, lines)
    };
    let plain = Style::new();
    // Round 2: F6's colours (wrong red, fix green), F9's dimmed tip, and a
    // coloured label or arrow; `badge` is the ERROR badge to use.
    let round = |badge: String, prefix: String, wrong: Style, fix: Style| {
        let message = mark_first(headline, wrong);
        let mut lines = Vec::new();
        for (name, line) in &p.lines {
            match name {
                Some("tip:") => {
                    let marked = mark_last(line, fix);
                    let marked = marked.replace(&format!("{fix:#}"), &format!("{fix:#}{dim}"));
                    lines.push(format!("{prefix} {dim}{marked}{dim:#}"));
                }
                Some("help:") => {}
                Some(name) => lines.push(format!("{name} {line}")),
                None => lines.push(line.clone()),
            }
        }
        frame(badge, message, lines)
    };
    let palette = || badge_of(problem_bg, "ERROR");
    let sbgo_badge = || {
        let b = Style::new()
            .bg_color(Some(anstyle::RgbColor(0xFF, 0x38, 0x8B).into()))
            .fg_color(Some(anstyle::RgbColor(0xFF, 0xFA, 0xF1).into()))
            .bold();
        format!("{b} ERROR {b:#}")
    };
    let painted = |style: Style, text: &str| format!("{style}{text}{style:#}");
    let accent = theme.style(Role::Accent);
    // Round 3 (R1 without dimming): an orange `hint` label, the wrong value
    // in the problem colour, fixes and possible values in green, and clap's
    // suggestion reworded so no line has two colons.
    let hint3 = |label: &str,
                 ask: &dyn Fn(&str, &str) -> String,
                 values: &dyn Fn(&[String]) -> Vec<String>| {
        let message = mark_first(headline, problem);
        let mut lines = Vec::new();
        for (name, line) in &p.lines {
            match name {
                Some("tip:") => match line.split_once(" exists: '") {
                    // "a similar subcommand exists: 'version'"
                    Some((similar, value)) => {
                        let kind = similar.rsplit(' ').next().unwrap_or("value");
                        let value = value.trim_end_matches('\'');
                        lines.push(format!("{label}{}", ask(kind, &painted(ok, value))));
                    }
                    None => lines.push(format!("{label}{}", mark_last(line, ok))),
                },
                Some("help:") => {}
                Some(name) => lines.push(format!("{name} {line}")),
                None => match line
                    .strip_prefix("[possible values: ")
                    .and_then(|v| v.strip_suffix(']'))
                {
                    Some(list) => {
                        let list: Vec<String> = list.split(", ").map(|v| painted(ok, v)).collect();
                        lines.extend(values(&list));
                    }
                    None => lines.push(line.clone()),
                },
            }
        }
        frame(badge_of(problem_bg, "ERROR"), message, lines)
    };
    let label_colon = painted(warn, "hint:") + " ";
    let label_column = painted(warn, "hint") + "  ";
    let label_chevron = painted(warn, "hint") + " › ";
    let ask_plain = |_: &str, v: &str| format!("did you mean '{v}'?");
    let ask_kind = |kind: &str, v: &str| {
        let noun = match kind {
            "subcommand" => "command",
            "argument" => "flag",
            other => other,
        };
        format!("did you mean the {noun} '{v}'?")
    };
    let or_list = |list: &[String]| match list.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => list.join(""),
    };
    match look {
        "hint-ask" => hint3(&label_colon, &ask_plain, &|l| {
            vec![format!("[possible values: {}]", l.join(", "))]
        }),
        "hint-ask-use" => hint3(&label_colon, &ask_plain, &|l| {
            vec![format!("{label_colon}use {}", or_list(l))]
        }),
        "hint-column" => hint3(&label_column, &ask_plain, &|l| {
            vec![format!("{label_column}one of {}", l.join(", "))]
        }),
        "hint-chevron" => hint3(&label_chevron, &ask_plain, &|l| {
            vec![format!("{label_chevron}use {}", or_list(l))]
        }),
        "hint-ask-kind" => hint3(&label_colon, &ask_kind, &|l| {
            vec![format!("[possible values: {}]", l.join(", "))]
        }),
        "hint-values-below" => hint3(&label_colon, &ask_plain, &|l| {
            vec![
                format!("{label_colon}possible values"),
                format!("      {}", l.join("  ")),
            ]
        }),
        "round-label" => round(palette(), painted(warn, "tip:"), problem, ok),
        "round-arrow" => round(palette(), painted(warn, "→"), problem, ok),
        "round-label-green" => round(palette(), painted(ok, "tip:"), problem, ok),
        "round-arrow-green" => round(palette(), painted(ok, "→"), problem, ok),
        "round-arrow-accent" => round(palette(), painted(accent, "→"), problem, ok),
        "round-label-sbgo" => round(sbgo_badge(), painted(warn, "tip:"), problem, ok),
        "round-arrow-sbgo" => round(sbgo_badge(), painted(warn, "→"), problem, ok),
        "round-label-bold" => round(palette(), painted(warn.bold(), "tip:"), problem, ok.bold()),
        "round-arrow-bold" => round(
            palette(),
            painted(warn.bold(), "→"),
            problem.bold(),
            ok.bold(),
        ),
        "round-label-underline" => round(palette(), painted(warn, "tip:"), problem.underline(), ok),
        "focus-wrong" => focus(Some(highlight), "tip: ", None, plain),
        "focus-both" => focus(Some(highlight), "tip: ", Some(highlight), plain),
        "focus-arrow-fix" => focus(None, "→ ", Some(highlight), plain),
        "focus-arrow-both" => focus(Some(highlight), "→ ", Some(highlight), plain),
        "focus-bare" => focus(Some(highlight), "", Some(highlight), plain),
        "focus-semantic" => focus(Some(problem), "tip: ", Some(ok), plain),
        "focus-arrow-semantic" => focus(Some(problem), "→ ", Some(ok), plain),
        "focus-bold" => focus(Some(bold), "tip: ", Some(bold), plain),
        "focus-dim" => focus(Some(highlight), "→ ", Some(highlight.bold()), dim),
        "focus-underline" => focus(Some(underline), "tip: ", Some(highlight), plain),
        "tip-tb-dark" => tip_frame(&|t| vec![format!("{} {t}", tip_badge(Role::OnAccent, "TIP"))]),
        "tip-tb-light" => tip_frame(&|t| vec![format!("{} {t}", tip_badge(Role::Text, "TIP"))]),
        "tip-tb-hint" => tip_frame(&|t| vec![format!("{} {t}", tip_badge(Role::OnAccent, "HINT"))]),
        "tip-tb-values" => tip_frame(&|t| {
            vec![format!(
                "{} {}",
                tip_badge(Role::OnAccent, "TIP"),
                quoted(t, Style::new().bold())
            )]
        }),
        "tip-eb-sbgo" => tip_frame_with(
            styled_badge(
                Some(anstyle::RgbColor(0xFF, 0x38, 0x8B).into()),
                Some(anstyle::RgbColor(0xFF, 0xFA, 0xF1).into()),
                "ERROR",
            ),
            &orange_label,
        ),
        "tip-eb-light" => tip_frame_with(
            styled_badge(problem_bg, theme.color(Role::Text), "ERROR"),
            &orange_label,
        ),
        "tip-eb-accent" => {
            tip_frame_with(badge_of(theme.color(Role::Accent), "ERROR"), &orange_label)
        }
        "tip-eb-kind" => tip_frame_with(badge_of(problem_bg, kind().0), &orange_label),
        "tip-label" => tip_frame(&|t| vec![format!("{warn}tip:{warn:#} {t}")]),
        "tip-line" => tip_frame(&|t| vec![format!("{warn}tip: {t}{warn:#}")]),
        "tip-values" => {
            tip_frame(&|t| vec![format!("{warn}tip:{warn:#} {}", quoted(t, warn_bold))])
        }
        "tip-values-cmd" => tip_frame(&|t| {
            vec![format!(
                "{warn_bold}tip:{warn_bold:#} {}",
                quoted(t, highlight)
            )]
        }),
        "tip-arrow" => tip_frame(&|t| vec![format!("{warn_bold}→{warn_bold:#} {t}")]),
        "tip-badge" => tip_frame(&|t| {
            let b = Style::new()
                .bg_color(theme.color(Role::Warn))
                .fg_color(theme.color(Role::OnAccent))
                .bold();
            vec![format!("{b} TIP {b:#} {t}")]
        }),
        "tip-bar" => tip_frame(&|t| vec![format!("{warn}▌{warn:#} {t}")]),
        "tip-cobra" => tip_frame(&|t| match t.split_once("exists: '") {
            Some((_, v)) => vec![
                format!("{warn_bold}Did you mean this?{warn_bold:#}"),
                format!("    {highlight}{}{highlight:#}", v.trim_end_matches('\'')),
            ],
            None => vec![format!("{warn}tip:{warn:#} {}", quoted(t, warn_bold))],
        }),
        "cobra" => frame(
            badge_of(problem_bg, "ERROR"),
            headline.clone(),
            did_you_mean(false, false),
        ),
        "cobra-help" => {
            let mut lines = did_you_mean(false, false);
            lines.push(format!(
                "{label}Run '{} --help' for usage.{label:#}",
                p.command
            ));
            frame(badge_of(problem_bg, "ERROR"), headline.clone(), lines)
        }
        "accent" => frame(
            badge_of(theme.color(Role::Accent), "ERROR"),
            headline.clone(),
            did_you_mean(false, false),
        ),
        "text" => frame(
            format!("{bold_problem}ERROR{bold_problem:#}"),
            headline.clone(),
            did_you_mean(false, false),
        ),
        "two-tone" => {
            let second = Style::new()
                .bg_color(theme.color(Role::CodeBg))
                .fg_color(theme.color(Role::Text));
            let (_, kind_text) = kind();
            frame(
                format!(
                    "{} {second} {kind_text} {second:#}",
                    badge_of(problem_bg, "ERROR")
                ),
                headline.clone(),
                did_you_mean(false, false),
            )
        }
        "block" => {
            let block = Style::new().bg_color(theme.color(Role::CodeBg));
            let width = headline.chars().count() + 4;
            let pad_row = if colour {
                format!("{block}{}{block:#}", " ".repeat(width))
            } else {
                String::new()
            };
            let mut out = format!(
                "{}\n  {}\n{}\n",
                " ".repeat(10),
                badge_of(problem_bg, "ERROR"),
                " ".repeat(10)
            );
            if colour {
                out.push_str(&format!("  {pad_row}\n"));
            }
            out.push_str(&format!("  {block}  {headline}  {block:#}\n"));
            if colour {
                out.push_str(&format!("  {pad_row}\n"));
            }
            let lines = did_you_mean(false, false);
            if !lines.is_empty() {
                out.push('\n');
                for line in lines {
                    out.push_str(&format!("  {line}\n"));
                }
            }
            out.push('\n');
            out
        }
        "kind" => frame(
            badge_of(problem_bg, kind().0),
            headline.clone(),
            did_you_mean(false, false),
        ),
        "one-line" => frame(
            badge_of(problem_bg, "ERROR"),
            headline.clone(),
            did_you_mean(true, true),
        ),
        "values" => frame(
            badge_of(problem_bg, "ERROR"),
            coloured_headline(),
            did_you_mean(false, true),
        ),
        "values-cobra" => frame(
            badge_of(problem_bg, "ERROR"),
            coloured_headline(),
            did_you_mean(true, true),
        ),
        "sbgo-exact" => {
            let pink = anstyle::RgbColor(0xFF, 0x38, 0x8B);
            let cream = anstyle::RgbColor(0xFF, 0xFA, 0xF1);
            let badge = Style::new()
                .bg_color(Some(pink.into()))
                .fg_color(Some(cream.into()))
                .bold();
            sbgo(badge, Style::new(), &[])
        }
        "sbgo-palette" => sbgo(palette_badge, Style::new(), &[]),
        "sbgo-light" => {
            let badge = Style::new()
                .bg_color(theme.color(Role::Problem))
                .fg_color(theme.color(Role::Text))
                .bold();
            sbgo(badge, Style::new(), &[])
        }
        "sbgo-tip" => sbgo(palette_badge, Style::new(), &tips),
        "sbgo-tip-help" => sbgo(palette_badge, Style::new(), &p.lines),
        "sbgo-red" => sbgo(palette_badge, problem, &[]),
        "badge-below" => format!(
            "\n  {badge} ERROR {badge:#}\n\n  {text}{headline}{text:#}\n\n{}\n",
            body("  ")
        ),
        "badge-inline" => format!(
            "\n  {badge} ERROR {badge:#} {text}{headline}{text:#}\n\n{}\n",
            body("          ")
        ),
        "heading" => format!(
            "\n  {bold_problem}ERROR{bold_problem:#}\n\n    {text}{headline}{text:#}\n\n{}\n",
            body("    ")
        ),
        "cross" => format!("{problem}✗{problem:#} {headline}\n{}", body("  ")),
        "bar" => {
            let bar = format!("{problem}▌{problem:#}");
            let mut out = format!("\n  {bar} {bold_problem}{headline}{bold_problem:#}\n");
            for line in body("").lines() {
                out.push_str(&format!("  {bar} {line}\n"));
            }
            out.push('\n');
            out
        }
        "rustc" => {
            let mut out = format!("{bold_problem}error{bold_problem:#}: {headline}\n");
            for (name, line) in &p.lines {
                let blue = theme.style(Role::Accent).bold();
                match name {
                    Some(name) => out.push_str(&format!(
                        "  {blue}={blue:#} {}{name}{:#} {line}\n",
                        Style::new().bold(),
                        Style::new().bold()
                    )),
                    None => out.push_str(&format!("  {blue}|{blue:#} {line}\n")),
                }
            }
            out
        }
        "usage" => {
            let block = Style::new().bg_color(theme.color(Role::CodeBg));
            let prog = theme.style(Role::Link).bg_color(theme.color(Role::CodeBg));
            let usage = format!("{} --help", p.command);
            let width = usage.len() + 4;
            let pad = if colour {
                format!("  {block}{}{block:#}\n", " ".repeat(width))
            } else {
                String::new()
            };
            let lines: String = p
                .lines
                .iter()
                .filter(|(name, _)| *name != Some("help:"))
                .map(|(name, line)| match name {
                    Some(name) => format!("  {label}{name}{label:#} {line}\n"),
                    None => format!("  {line}\n"),
                })
                .collect();
            format!(
                "\n  {badge} ERROR {badge:#}\n\n  {text}{headline}{text:#}\n\n{lines}\n  {label}For help:{label:#}\n\n{pad}  {block}  {block:#}{prog}{usage}{prog:#}{block}  {block:#}\n{pad}\n"
            )
        }
        _ => unreachable!("unknown error look"),
    }
}

/// `errors [page|look] [count]`: a page (`sbgo`, `tips`, `focus`), one look,
/// or everything; a look with a `count` prints just its first `count` cases
/// with no heading (for capturing).
fn errors_gallery<W: Write>(
    out: &mut W,
    only: Option<&str>,
    count: Option<usize>,
    colour: bool,
) -> std::io::Result<()> {
    let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
    let nodes = tree();
    let rule = Style::new().dimmed();
    // `sbgo`: every look in sb-go's frame (A-J, 8-13, rounds R and H), each
    // on the typo error alone;
    // `tips`: the tip variants, on the two errors that carry a tip.
    if only == Some("focus") {
        for (look, title) in ERROR_LOOKS.iter().filter(|(l, _)| l.starts_with("focus-")) {
            writeln!(out, "\n{rule}━━━━ {title} ━━━━{rule:#}")?;
            for argv in ERROR_CASES {
                let err = build(Look::Clap, &theme, &nodes, "sb")
                    .try_get_matches_from(argv.iter().copied())
                    .expect_err("a parse error");
                write!(
                    out,
                    "{}",
                    draw_error(look, &error_parts(err), &theme, colour)
                )?;
            }
        }
        return Ok(());
    }
    if only == Some("tips") {
        // The tip treatments first, then TIP badges, then ERROR badges.
        let group = |l: &str| match () {
            () if l.starts_with("tip-tb-") => 1,
            () if l.starts_with("tip-eb-") => 2,
            () => 0,
        };
        let mut looks: Vec<_> = ERROR_LOOKS
            .iter()
            .filter(|(l, _)| l.starts_with("tip-"))
            .collect();
        looks.sort_by_key(|(l, _)| group(l));
        for (look, title) in looks {
            writeln!(out, "\n{rule}━━━━ {title} ━━━━{rule:#}")?;
            for argv in &ERROR_CASES[..2] {
                let err = build(Look::Clap, &theme, &nodes, "sb")
                    .try_get_matches_from(argv.iter().copied())
                    .expect_err("a parse error");
                write!(
                    out,
                    "{}",
                    draw_error(look, &error_parts(err), &theme, colour)
                )?;
            }
        }
        return Ok(());
    }
    let compact = only == Some("sbgo");
    for (look, title) in ERROR_LOOKS {
        let sbgo_style = (title.starts_with(|c: char| c.is_ascii_uppercase())
            && !look.starts_with("tip-")
            && !look.starts_with("focus-"))
            || look.starts_with("sbgo-");
        if compact && !sbgo_style || !compact && only.is_some_and(|o| o != look) {
            continue;
        }
        if count.is_none() {
            writeln!(out, "\n{rule}━━━━ {title} ━━━━{rule:#}")?;
        }
        let cases = match (compact, count) {
            (true, _) => &ERROR_CASES[..1],
            (false, Some(n)) => &ERROR_CASES[..n.min(ERROR_CASES.len())],
            (false, None) => &ERROR_CASES[..],
        };
        for argv in cases {
            let err = build(Look::Clap, &theme, &nodes, "sb")
                .try_get_matches_from(argv.iter().copied())
                .expect_err("a parse error");
            write!(
                out,
                "{}",
                draw_error(look, &error_parts(err), &theme, colour)
            )?;
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Colour follows anstream (NO_COLOR, a terminal or not).
    let mut out = anstream::stdout();
    let colour = anstream::AutoStream::choice(&std::io::stdout()) != anstream::ColorChoice::Never;
    let rule = Style::new().dimmed();
    match args.split_first() {
        // `looks`: every error look as `name<TAB>title` (for the page builder).
        Some((which, _)) if which == "looks" => {
            for (look, title) in ERROR_LOOKS {
                writeln!(out, "{look}\t{title}")?;
            }
        }
        Some((which, rest)) if which == "errors" => {
            let count = rest.get(1).and_then(|c| c.parse().ok());
            errors_gallery(&mut out, rest.first().map(String::as_str), count, colour)?;
        }
        Some((which, path)) => {
            let Some((look, _, _)) = LOOKS.iter().find(|(_, name, _)| name == which) else {
                panic!("looks: clap, palette, cargo, template, clap-help, sb-go");
            };
            render(*look, path, &mut out, colour)?;
        }
        None => {
            for (look, _, title) in LOOKS {
                writeln!(out, "\n{rule}━━━━ {title} ━━━━{rule:#}")?;
                render(look, &[], &mut out, colour)?;
                if look == Look::ClapHelp || look == Look::SbGo {
                    writeln!(out, "\n{rule}━━━━ {title}: install ━━━━{rule:#}")?;
                    render(look, &["install".to_string()], &mut out, colour)?;
                }
            }
        }
    }
    Ok(())
}
