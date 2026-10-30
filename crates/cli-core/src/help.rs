//! Help in sb-go's (fang's) format, in sb's palette (cli-core spec: Help and
//! parse errors; chosen by the maintainer from proto-help's comparison).
//!
//! clap still parses and still prints help: each command's text is set with
//! clap's `Command::override_help`
//! (https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.override_help),
//! so `-h`, `--help`, `sb help <command>`, the colour choice (clap strips
//! styles when colour is off) and the exit code stay clap's. Why not clap's
//! own renderer with styles or a template: its command list shows names
//! only (no usage hints), its usage says `[OPTIONS]` and its lists are
//! indented its own way (proto-help FINDINGS.md). The layout follows fang
//! v2.0.1 `help.go`; widths are display widths (unicode-width), padded by
//! hand since `{:<w$}` counts chars and clap's width helper is private.
//!
//! Differences from fang: clap trims the blank line fang prints before and
//! after help; the about isn't wrapped at 120 columns yet (tasks/todo.md);
//! `sb help help` is clap's own, since clap adds that subcommand after
//! `apply` runs. clap settings fang's format has no place for (short-only
//! help texts, `hide_default_value`, `last` positionals, visible aliases)
//! are not shown.

use anstyle::Style;
use clap::builder::StyledStr;
use clap::{Arg, Command};
use unicode_width::UnicodeWidthStr;

use crate::theme::{Role, Theme};

/// clap's own text for its `help` subcommand (clap_builder 4.6.7
/// `_check_help_and_version`), which sb keeps listed (maintainer).
const HELP_SUBCOMMAND_ABOUT: &str = "Print this message or the help of the given subcommand(s)";

/// Each command in `cmd` with its help replaced by sb's format. `colour`
/// says whether help will be shown in colour (the shaded usage block's
/// padding rows are left out otherwise, as fang does).
///
/// Why not read the tree after `Command::build()`, which fills in command
/// paths, inherited globals and clap's help flag and subcommand: a built
/// command lists the help flag and every inherited global among its own
/// arguments, so fang's `[--flags]` rule (own flags only) couldn't tell them
/// apart without comparing each command with its parent, which costs as
/// much as passing the inherited flags down here.
pub fn apply(cmd: Command, theme: &Theme, colour: bool) -> Command {
    let path = cmd.get_name().to_string();
    apply_at(cmd, &path, &[], theme, colour)
}

fn apply_at(cmd: Command, path: &str, inherited: &[Arg], theme: &Theme, colour: bool) -> Command {
    let refs: Vec<&Arg> = inherited.iter().collect();
    let text = render(&cmd, path, &refs, theme, colour);
    let mut below = inherited.to_vec();
    below.extend(cmd.get_arguments().filter(|a| a.is_global_set()).cloned());
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    let mut cmd = cmd.override_help(StyledStr::from(text));
    for name in names {
        let sub_path = format!("{path} {name}");
        cmd = cmd.mut_subcommand(&name, |sub| apply_at(sub, &sub_path, &below, theme, colour));
    }
    cmd
}

/// Whether clap will add its `help` subcommand (it does for any command
/// with subcommands unless switched off).
fn has_help_subcommand(cmd: &Command) -> bool {
    cmd.has_subcommands() && !cmd.is_disable_help_subcommand_set()
}

/// fang's usage hint after a command's name: `[command]` when it has
/// subcommands, then its arguments (`[name]` optional, `<name>` required),
/// then `[--flags]` when it has flags of its own (clap's `--help` and
/// inherited flags don't count, as cobra's don't in fang's list).
fn hint(cmd: &Command) -> String {
    let mut hint = String::new();
    if cmd.get_subcommands().any(|c| !c.is_hide_set()) {
        hint.push_str(" [command]");
    }
    for arg in cmd.get_positionals().filter(|a| !a.is_hide_set()) {
        // Lower case, as sb-go's hints (`[branch_name]`); clap's derive names
        // positionals in SCREAMING_SNAKE.
        let name = arg
            .get_value_names()
            .and_then(|names| names.first())
            .map_or_else(|| arg.get_id().to_string(), ToString::to_string)
            .to_lowercase();
        if arg.is_required_set() {
            hint.push_str(&format!(" <{name}>"));
        } else {
            hint.push_str(&format!(" [{name}]"));
        }
    }
    if cmd
        .get_arguments()
        .any(|a| !a.is_positional() && !a.is_hide_set())
    {
        hint.push_str(" [--flags]");
    }
    hint
}

/// A flag as fang shows it: `-s --long` (no value placeholder), its help,
/// and its default as ` (value)` unless empty, false, 0 or [].
fn flag_row(arg: &Arg) -> (String, String, String) {
    let long = arg.get_long().unwrap_or_default().to_string();
    let shown = match (arg.get_short(), arg.get_long()) {
        (Some(short), Some(long)) => format!("-{short} --{long}"),
        (Some(short), None) => format!("-{short}"),
        (None, _) => format!("--{long}"),
    };
    let mut help = arg.get_help().map(ToString::to_string).unwrap_or_default();
    let default = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy())
        .collect::<Vec<_>>()
        .join(",");
    if !matches!(default.as_str(), "" | "false" | "0" | "[]") {
        help.push_str(&format!(" ({default})"));
    }
    let key = if long.is_empty() { shown.clone() } else { long };
    (key, shown, help)
}

/// The help text for `cmd`, reached through `path` (`sb install`), with
/// `inherited` the global flags of its ancestors.
fn render(cmd: &Command, path: &str, inherited: &[&Arg], theme: &Theme, colour: bool) -> String {
    let mut commands: Vec<(String, String, String)> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| {
            let about = c.get_about().map(ToString::to_string).unwrap_or_default();
            (c.get_name().to_string(), hint(c), about)
        })
        .collect();
    if has_help_subcommand(cmd) {
        commands.push((
            "help".into(),
            " [command]".into(),
            HELP_SUBCOMMAND_ABOUT.into(),
        ));
    }
    commands.sort();
    let mut flags: Vec<(String, String, String)> = cmd
        .get_arguments()
        .chain(inherited.iter().copied())
        .filter(|a| !a.is_positional() && !a.is_hide_set())
        .map(flag_row)
        .collect();
    if !cmd.is_disable_help_flag_set() {
        flags.push((
            "help".into(),
            "-h --help".into(),
            format!("Help for {}", cmd.get_name()),
        ));
    }
    flags.sort();
    let width = commands
        .iter()
        .map(|(name, hint, _)| name.width() + hint.width())
        .chain(flags.iter().map(|(_, shown, _)| shown.width()))
        .max()
        .unwrap_or(0)
        + 2;

    let text = theme.style(Role::Text);
    // Bold only with colour: the plain theme (`ascii`) writes no styling.
    let heading = match theme.color(Role::Accent) {
        Some(_) => theme.style(Role::Accent).bold(),
        None => Style::new(),
    };
    let name_style = theme.style(Role::Highlight);
    let muted = theme.style(Role::Muted);
    let flag = theme.style(Role::Ok);

    // fang shows the long description, else the short one.
    let about = cmd
        .get_long_about()
        .or(cmd.get_about())
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut out = String::new();
    for line in about.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&format!("  {text}{line}{text:#}\n"));
        }
    }
    out.push_str(&format!("\n  {heading}USAGE{heading:#}\n\n"));
    out.push_str(&usage_block(cmd, path, theme, colour));
    if !commands.is_empty() {
        out.push_str(&format!("\n  {heading}COMMANDS{heading:#}\n\n"));
        for (name, hint, about) in &commands {
            let pad = " ".repeat(width - name.width() - hint.width());
            out.push_str(&format!(
                "    {name_style}{name}{name_style:#}{muted}{hint}{muted:#}{pad}{text}{about}{text:#}\n"
            ));
        }
    }
    out.push_str(&format!("\n  {heading}FLAGS{heading:#}\n\n"));
    for (_, shown, help) in &flags {
        let pad = " ".repeat(width - shown.width());
        out.push_str(&format!(
            "    {flag}{shown}{flag:#}{pad}{text}{help}{text:#}\n"
        ));
    }
    out
}

/// The usage line, `[--flags]` always last (cobra's own `-h` counts for the
/// command shown). With colour it sits in a shaded block with a padding row
/// above and below: the program in the link colour, the command path in the
/// command colour, the hints muted.
fn usage_block(cmd: &Command, path: &str, theme: &Theme, colour: bool) -> String {
    let hint = format!("{} [--flags]", hint(cmd).trim_end_matches(" [--flags]"));
    let (program, rest) = path.split_at(path.find(' ').unwrap_or(path.len()));
    if !colour {
        return format!("    {path}{hint}\n");
    }
    let bg = theme.color(Role::CodeBg);
    let block = Style::new().bg_color(bg);
    let program_style = theme.style(Role::Link).bg_color(bg);
    let path_style = theme.style(Role::Highlight).bg_color(bg);
    let hint_style = theme.style(Role::Muted).bg_color(bg);
    let pad = format!(
        "  {block}{}{block:#}\n",
        " ".repeat(path.width() + hint.width() + 4)
    );
    format!(
        "{pad}  {block}  {block:#}{program_style}{program}{program_style:#}{path_style}{rest}{path_style:#}{hint_style}{hint}{hint_style:#}{block}  {block:#}\n{pad}"
    )
}

/// A clap parse error in sb-go's (fang's) layout, in sb's palette
/// (maintainer, 2026-10-30): an `ERROR` badge on the problem colour, a blank
/// line, clap's message, then clap's tips and context lines (`[possible
/// values: …]`) and where to look, naming the command that failed
/// (`help: try 'sb version --help'`), or `program` when clap doesn't say
/// (a bad value carries no usage). clap's usage paragraph is left out
/// (help has it): removed with clap's `Error::remove(ContextKind::Usage)`
/// (https://docs.rs/clap/4.6.7/clap/error/struct.Error.html#method.remove),
/// and skipped by line for errors whose usage is part of their message
/// (`Error::format`ted ones, which sb doesn't produce today). The command's
/// name is read from clap's usage format; a free-text `override_usage`
/// would give a wrong one.
///
/// Why not a clap `ErrorFormatter`
/// (https://docs.rs/clap/4.6.7/clap/error/trait.ErrorFormatter.html): clap
/// builds its messages in crate-private code (`write_dynamic_context`,
/// `did_you_mean`, `try_help` in clap_builder 4.6.7 `error/format.rs`), so a
/// formatter of ours would have to rewrite every message, and clap skips it
/// for errors that carry their own message. This keeps clap's wording and
/// re-lays it out from its plain rendering (`StyledStr`'s Display drops
/// styles). It keys on clap's English (`error: `, `tip: `, `Usage:`, `For
/// more information`); the tests pin each shape, so a clap update that
/// changes one fails them.
pub fn parse_error_block(mut err: clap::Error, program: &str, theme: &Theme) -> String {
    use clap::error::{ContextKind, ContextValue};
    let command = match err.remove(ContextKind::Usage) {
        Some(ContextValue::StyledStr(usage)) => command_path(&usage.to_string()),
        _ => None,
    };
    let text = err.render().to_string();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let headline = first.strip_prefix("error: ").unwrap_or(first);
    let mut body: Vec<(Option<&str>, String)> = Vec::new();
    let mut in_usage = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            in_usage = false;
        } else if trimmed.starts_with("Usage:") {
            in_usage = true;
        } else if let Some(pointer) = trimmed.strip_prefix("For more information, ") {
            // clap names the help it offers (`'--help'`, a custom flag, or
            // the `help` subcommand); a flag gets the failing command's name.
            let target = pointer
                .trim_end_matches('.')
                .strip_prefix("try '")
                .and_then(|t| t.strip_suffix('\''));
            let pointer = match target {
                Some(flag) if flag.starts_with('-') => {
                    format!("try '{} {flag}'", command.as_deref().unwrap_or(program))
                }
                _ => pointer.trim_end_matches('.').to_string(),
            };
            body.push((Some("help:"), pointer));
        } else if in_usage {
            continue;
        } else if let Some(tip) = trimmed.strip_prefix("tip: ") {
            body.push((Some("tip:"), tip.to_string()));
        } else {
            body.push((None, trimmed.to_string()));
        }
    }
    // The badge text takes the on-accent colour, the palette's dark text
    // for a bright background (the theme's `text` is light and wouldn't
    // read on the problem colour).
    let badge = match theme.color(Role::Problem) {
        Some(bg) => Style::new()
            .bg_color(Some(bg))
            .fg_color(theme.color(Role::OnAccent))
            .bold(),
        None => Style::new(),
    };
    let (text, label) = (theme.style(Role::Text), theme.style(Role::Muted));
    let mut out = format!("\n  {badge} ERROR {badge:#}\n\n  {text}{headline}{text:#}\n");
    if !body.is_empty() {
        out.push('\n');
    }
    for (name, line) in body {
        match name {
            Some(name) => out.push_str(&format!("  {label}{name}{label:#} {line}\n")),
            None => out.push_str(&format!("  {line}\n")),
        }
    }
    out.push('\n');
    out
}

/// The command a usage line is for: `Usage: sb version [OPTIONS]` gives
/// `sb version` (the words before the first argument, option or
/// placeholder).
fn command_path(usage: &str) -> Option<String> {
    let first = usage.lines().next()?.trim();
    let words: Vec<&str> = first
        .strip_prefix("Usage:")
        .unwrap_or(first)
        .split_whitespace()
        .take_while(|w| !w.starts_with(['[', '<', '-']))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

#[cfg(test)]
mod tests {
    use clap::{Arg, ArgAction};

    use super::*;
    use crate::theme::{Palette, Profile};

    fn sb() -> Command {
        Command::new("sb")
            .about("Saltbox CLI")
            .arg(
                Arg::new("color")
                    .long("color")
                    .global(true)
                    .default_value("auto")
                    .help("Controls when to use color"),
            )
            .subcommand(
                Command::new("version")
                    .about("Print Saltbox CLI version")
                    .arg(
                        Arg::new("json")
                            .long("json")
                            .action(ArgAction::SetTrue)
                            .help("Print machine-readable version information"),
                    ),
            )
            .subcommand(
                Command::new("install")
                    .about("Runs Ansible playbooks with specified tags")
                    .long_about("Runs Ansible playbooks.\n\nTags select what runs.")
                    .arg(Arg::new("tags").value_name("tags").num_args(0..))
                    .arg(
                        Arg::new("skip-tags")
                            .short('s')
                            .long("skip-tags")
                            .help("Tags to skip"),
                    ),
            )
            .subcommand(Command::new("secret").about("Hidden").hide(true))
    }

    fn plain(cmd: &Command, path: &str, inherited: &[&Arg]) -> String {
        render(cmd, path, inherited, &Theme::plain(), false)
    }

    /// sb-go's layout: about, headings, usage, commands sorted with fang's
    /// hints (clap's `help` subcommand listed), flags with defaults and
    /// `-h --help`, all in one column; hidden commands left out.
    #[test]
    fn the_root_follows_sb_gos_format() {
        assert_eq!(
            plain(&sb(), "sb", &[]),
            "  Saltbox CLI\n\n  USAGE\n\n    sb [command] [--flags]\n\n  COMMANDS\n\n    help [command]            Print this message or the help of the given subcommand(s)\n    install [tags] [--flags]  Runs Ansible playbooks with specified tags\n    version [--flags]         Print Saltbox CLI version\n\n  FLAGS\n\n    --color                   Controls when to use color (auto)\n    -h --help                 Help for sb\n"
        );
    }

    /// A subcommand: its long about (indented), its usage with arguments
    /// and `[--flags]`, its own flags plus the inherited global ones.
    #[test]
    fn a_subcommand_shows_its_long_about_and_inherited_flags() {
        let root = sb();
        let color = root
            .get_arguments()
            .find(|a| a.get_id() == "color")
            .expect("color");
        let install = root.find_subcommand("install").expect("install");
        assert_eq!(
            plain(install, "sb install", &[color]),
            "  Runs Ansible playbooks.\n\n  Tags select what runs.\n\n  USAGE\n\n    sb install [tags] [--flags]\n\n  FLAGS\n\n    --color         Controls when to use color (auto)\n    -h --help       Help for install\n    -s --skip-tags  Tags to skip\n"
        );
    }

    /// With colour: the usage line sits in a shaded block with a padding
    /// row above and below, and every colour is the theme's.
    #[test]
    fn colour_adds_the_shaded_usage_block() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let text = render(&sb(), "sb", &[], &theme, true);
        let lines: Vec<&str> = text.lines().collect();
        let usage = lines
            .iter()
            .position(|l| l.contains("[command]"))
            .expect("usage line");
        assert!(
            lines[usage - 1].contains("\x1b[48;2;"),
            "{:?}",
            lines[usage - 1]
        );
        assert!(
            lines[usage + 1].contains("\x1b[48;2;"),
            "{:?}",
            lines[usage + 1]
        );
        assert!(
            text.contains("\x1b[1;38;2;") || text.contains("\x1b[1m"),
            "{text:?}"
        );
        for named in 30..=37 {
            assert!(
                !text.contains(&format!("\x1b[{named}m")),
                "named colour in {text:?}"
            );
        }
    }

    /// `apply` sets every command's help, so clap prints sb's text for
    /// `--help` at any level and for `help <command>`.
    #[test]
    fn apply_replaces_every_commands_help() {
        let mut cmd = apply(sb(), &Theme::plain(), false);
        let root = cmd.render_help().to_string();
        assert!(root.contains("  USAGE\n"), "{root}");
        let install = cmd
            .find_subcommand_mut("install")
            .expect("install")
            .render_help()
            .to_string();
        assert!(
            install.contains("    sb install [tags] [--flags]\n"),
            "{install}"
        );
        let via_help = apply(sb(), &Theme::plain(), false)
            .try_get_matches_from(["sb", "help", "version"])
            .expect_err("help is shown as an error")
            .to_string();
        assert!(
            via_help.contains("    sb version [--flags]\n"),
            "{via_help}"
        );
    }

    /// The cases sb doesn't have yet but will: a required positional, an
    /// argument without a value name (its id), a short-only flag, a hidden
    /// argument, and a multi-value default.
    #[test]
    fn arguments_and_flags_of_every_shape() {
        let cmd = Command::new("generate")
            .about("Generate a completion script")
            .arg(Arg::new("shell").value_name("bash|zsh|fish").required(true))
            .arg(Arg::new("target"))
            .arg(Arg::new("secret").hide(true))
            .arg(
                Arg::new("quiet")
                    .short('q')
                    .action(ArgAction::SetTrue)
                    .help("Say less"),
            )
            .arg(Arg::new("internal").long("internal").hide(true))
            .arg(
                Arg::new("tags")
                    .long("tags")
                    .num_args(1..)
                    .default_values(["a", "b"])
                    .help("Tags"),
            );
        assert_eq!(
            plain(&cmd, "sb completion generate", &[]),
            "  Generate a completion script\n\n  USAGE\n\n    sb completion generate <bash|zsh|fish> [target] [--flags]\n\n  FLAGS\n\n    -q         Say less\n    -h --help  Help for generate\n    --tags     Tags (a,b)\n"
        );
    }

    /// clap's derive names positionals in SCREAMING_SNAKE (`TAGS`); sb-go's
    /// hints are lower case (`[tags]`, `[branch_name]`).
    #[test]
    fn derived_value_names_read_as_sb_gos() {
        #[derive(clap::Parser)]
        struct Branch {
            /// Branch to use
            branch_name: Option<String>,
        }
        let cmd = <Branch as clap::CommandFactory>::command().name("branch");
        assert!(
            plain(&cmd, "sb branch", &[]).contains("    sb branch [branch_name] [--flags]\n"),
            "{}",
            plain(&cmd, "sb branch", &[])
        );
    }

    /// Without clap's help flag or help subcommand, neither is listed.
    #[test]
    fn switched_off_help_is_not_listed() {
        let cmd = Command::new("sb")
            .about("Saltbox CLI")
            .disable_help_flag(true)
            .disable_help_subcommand(true)
            .subcommand(Command::new("version").about("Print Saltbox CLI version"));
        let text = plain(&cmd, "sb", &[]);
        assert!(!text.contains("help"), "{text}");
    }

    /// Globals reach every depth: a command two levels down lists the
    /// root's and its parent's global flags.
    #[test]
    fn globals_are_inherited_at_every_depth() {
        let cmd = Command::new("sb")
            .about("Saltbox CLI")
            .arg(Arg::new("color").long("color").global(true).help("Colour"))
            .subcommand(
                Command::new("docker")
                    .about("Docker")
                    .arg(
                        Arg::new("verbose")
                            .long("verbose")
                            .global(true)
                            .action(ArgAction::SetTrue)
                            .help("More"),
                    )
                    .subcommand(Command::new("ps").about("List containers")),
            );
        let mut cmd = apply(cmd, &Theme::plain(), false);
        let ps = cmd
            .find_subcommand_mut("docker")
            .and_then(|d| d.find_subcommand_mut("ps"))
            .expect("ps")
            .render_help()
            .to_string();
        assert!(ps.contains("    --color    Colour\n"), "{ps}");
        assert!(ps.contains("    --verbose  More\n"), "{ps}");
        assert!(ps.contains("    sb docker ps [--flags]\n"), "{ps}");
    }

    /// The `help` row copies clap's own text; this catches clap changing it.
    #[test]
    fn the_help_row_matches_claps_help_subcommand() {
        let mut cmd = sb();
        cmd.build();
        let about = cmd
            .find_subcommand("help")
            .and_then(Command::get_about)
            .map(ToString::to_string);
        assert_eq!(about.as_deref(), Some(HELP_SUBCOMMAND_ABOUT));
    }

    /// A hidden flag doesn't earn a command `[--flags]` in the list.
    #[test]
    fn hidden_flags_dont_count_towards_the_hint() {
        let cmd = Command::new("sb").about("Saltbox CLI").subcommand(
            Command::new("fact")
                .about("Manage facts")
                .arg(Arg::new("debug").long("debug").hide(true)),
        );
        let text = plain(&cmd, "sb", &[]);
        assert!(text.contains("    fact  "), "{text}");
        assert!(!text.contains("fact [--flags]"), "{text}");
    }

    /// clap's usage paragraph, every line of it, is left out; tips and
    /// context lines stay; clap's pointer becomes `help:`.
    #[test]
    fn a_parse_error_keeps_claps_message_without_its_usage() {
        let err = clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            "invalid value 'x'\n  [possible values: a, b]\n\n  tip: try 'a'\n\nUsage: sb one\n       sb two\n\nFor more information, try '--help'.\n",
        );
        assert_eq!(
            parse_error_block(err, "sb", &Theme::plain()),
            "\n   ERROR \n\n  invalid value 'x'\n\n  [possible values: a, b]\n  tip: try 'a'\n  help: try 'sb --help'\n\n"
        );
    }

    /// With colour the badge sits on a background (the problem colour) and
    /// the labels are muted.
    #[test]
    fn a_parse_error_takes_the_themes_colours() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let err = Command::new("sb")
            .subcommand(Command::new("version"))
            .try_get_matches_from(["sb", "nope"])
            .expect_err("unknown command");
        let block = parse_error_block(err, "sb", &theme);
        assert!(
            block.starts_with(&format!(
                "\n  {badge} ERROR {badge:#}\n",
                badge = Style::new()
                    .bg_color(theme.color(Role::Problem))
                    .fg_color(theme.color(Role::OnAccent))
                    .bold()
            )),
            "{block:?}"
        );
        assert!(
            block.contains(&format!("{}help:", theme.style(Role::Muted))),
            "{block:?}"
        );
        let typo = Command::new("sb")
            .subcommand(Command::new("version"))
            .try_get_matches_from(["sb", "verison"])
            .expect_err("typo");
        let block = parse_error_block(typo, "sb", &theme);
        assert!(
            block.contains(&format!("{}tip:", theme.style(Role::Muted))),
            "{block:?}"
        );
    }

    fn plain_error(cmd: Command, argv: &[&str]) -> String {
        parse_error_block(
            cmd.try_get_matches_from(argv).expect_err("a parse error"),
            "sb",
            &Theme::plain(),
        )
    }

    /// clap's real output for the kinds with extra lines, so a clap update
    /// that changes their shape fails here: missing arguments, a conflict
    /// with several arguments, a missing subcommand, a multi-line usage, and
    /// the help pointer naming the command that failed.
    #[test]
    fn claps_multi_line_errors_keep_their_lines() {
        let add = Command::new("sb").subcommand(
            Command::new("add")
                .arg(Arg::new("name").required(true))
                .arg(Arg::new("path").required(true)),
        );
        assert_eq!(
            plain_error(add, &["sb", "add"]),
            "\n   ERROR \n\n  the following required arguments were not provided:\n\n  <name>\n  <path>\n  help: try 'sb add --help'\n\n"
        );
        let flag = |id: &'static str| Arg::new(id).long(id).action(ArgAction::SetTrue);
        let conflict = Command::new("sb")
            .arg(flag("a"))
            .arg(flag("b"))
            .arg(flag("c").conflicts_with_all(["a", "b"]));
        assert_eq!(
            plain_error(conflict, &["sb", "--c", "--a", "--b"]),
            "\n   ERROR \n\n  the argument '--c' cannot be used with:\n\n  --a\n  --b\n  help: try 'sb --help'\n\n"
        );
        let required = Command::new("sb")
            .subcommand_required(true)
            .arg(flag("verbose"))
            .subcommand(Command::new("version"));
        assert_eq!(
            plain_error(required, &["sb", "--verbose"]),
            "\n   ERROR \n\n  'sb' requires a subcommand but one was not provided\n\n  [subcommands: version, help]\n  help: try 'sb --help'\n\n"
        );
        let two_usages = Command::new("sb")
            .args_conflicts_with_subcommands(true)
            .arg(Arg::new("x").long("x").required(true))
            .subcommand(Command::new("version"));
        assert_eq!(
            plain_error(two_usages, &["sb", "--bogus"]),
            "\n   ERROR \n\n  unexpected argument '--bogus' found\n\n  help: try 'sb --help'\n\n"
        );
    }

    /// Without clap's help flag there is nothing to point at, so no `help:`.
    #[test]
    fn no_help_flag_means_no_help_line() {
        let cmd = Command::new("sb")
            .disable_help_flag(true)
            .disable_help_subcommand(true)
            .arg(
                Arg::new("verbose")
                    .long("verbose")
                    .action(ArgAction::SetTrue),
            );
        assert_eq!(
            plain_error(cmd, &["sb", "--bogus"]),
            "\n   ERROR \n\n  unexpected argument '--bogus' found\n\n"
        );
    }

    /// clap names the help it can offer: with the flag off but the help
    /// subcommand on, that's `help`, kept as clap says it.
    #[test]
    fn the_pointer_keeps_claps_help_target() {
        let cmd = Command::new("sb")
            .disable_help_flag(true)
            .subcommand(Command::new("version"));
        assert_eq!(
            plain_error(cmd, &["sb", "--bogus"]),
            "\n   ERROR \n\n  unexpected argument '--bogus' found\n\n  help: try 'help'\n\n"
        );
    }
}
