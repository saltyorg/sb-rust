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

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Colour follows anstream (NO_COLOR, a terminal or not).
    let mut out = anstream::stdout();
    let colour = anstream::AutoStream::choice(&std::io::stdout()) != anstream::ColorChoice::Never;
    let rule = Style::new().dimmed();
    match args.split_first() {
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
