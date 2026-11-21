//! Help in sb-go's (fang's) format, in sb's palette (cli-core spec: Help and
//! parse errors; chosen by the maintainer from proto-help's comparison).
//!
//! clap still parses and still prints help: each command's text is set as
//! its `Command::before_help`, the whole of a `{before-help}` help template
//! (<https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.help_template>,
//! <https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.before_help>),
//! so `-h`, `--help`, `sb help <command>`, the colour choice (clap strips
//! styles when colour is off) and the exit code stay clap's. A command with
//! a shown flag that has a long help (`Arg::long_help`) also gets a long
//! text, its `Command::before_long_help`
//! (<https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.before_long_help>):
//! clap prints it for `--help` and `sb help <command>`, and the short one
//! for `-h`, as it does for its own help.
//!
//! Why not `Command::override_help`: clap prints that one text for `-h`
//! and `--help` alike (`clap_builder` 4.6.7 `src/output/help.rs`,
//! `write_help`). What the template way costs, from `clap_builder`
//! 4.6.7's `src/output/help_template.rs` and `styled_str.rs`: clap adds a
//! blank line after the text, which its own trim at the end of
//! `write_help` removes again, so the output is the same; a literal `{n}`
//! in a text would become a new line (sb's texts have none; sb's unit
//! tests, `crates/sb/src/main.rs`, check); and clap would wrap the text
//! again, from column 0, if its `wrap_help` feature were on (it is off;
//! sb's help tests check that continuation lines keep their indent).
//!
//! Every line is wrapped to [`width`], fang's: the terminal's width capped
//! at 120 columns (fang v2.0.1 `help.go`, `width`). The about wraps under
//! its two-space indent, as fang's `Width(width()).PaddingLeft(2)` does; a
//! command's or a flag's text wraps under the help column, which fang
//! leaves to the terminal (maintainer, 2026-11-14). Why not clap's own wrap
//! (`wrap_help`, `Command::term_width`, `Command::max_term_width`,
//! <https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.max_term_width>):
//! those only size clap's own layout. A `before_help` text is wrapped
//! whole from column 0 (`clap_builder` 4.6.7 `write_before_help`), clap's
//! width probe is private (`dimensions`), and the feature would add the
//! `terminal_size` crate.
//!
//! Why not clap's own renderer with styles or a template: its command list shows names
//! only (no usage hints), its usage says `[OPTIONS]` and its lists are
//! indented its own way (docs/prototypes/proto-help.md). The layout follows fang
//! v2.0.1 `help.go`; widths are display widths (unicode-width), padded by
//! hand since `{:<w$}` counts chars and clap's width helper is private.
//! Why not a borderless two-column tabled `Table` with `Width::wrap` on
//! the text column, for the rows: each row's lead carries its own styles,
//! the key column's width is fang's (widest key plus two, across commands
//! and flags), and tabled's word wrap would keep the space it carries to a
//! piece's start, shifting some continuation lines a column right.
//!
//! Differences from fang: help ends with its last row, where fang prints
//! a blank line after it (clap trims the end; the blank line before the
//! about is kept, `LEAD`); fang pads the about's lines with spaces to the
//! width, sb doesn't; command and flag rows wrap under their help column and drop
//! their text's blank lines (fang shows a paragraph break); the usage
//! line isn't wrapped (sb's longest is far below 80 columns); with no
//! terminal, `COLUMNS` gives the width (clap's order; fang reads only the
//! terminal), and a PTY nobody sized counts as none (fang's `min(0, 120)`
//! doesn't wrap); a row's text keeps at least 20 columns; and the wrap
//! itself is tabled's, which differs from fang's (charmbracelet/x/ansi
//! `Wrap`) in two rare cases: the line after one that fills the width
//! exactly breaks a column early, and fang also breaks after a hyphen and
//! starts a word wider than the width on a fresh line. `sb help help` is
//! clap's own, since clap adds that subcommand after `apply` runs. clap settings fang's format has no place for (short-only
//! help texts, `hide_default_value`, `last` positionals, visible aliases)
//! are not shown.

use std::borrow::Cow;
use std::fmt::Write as _;

use anstyle::Style;
use clap::builder::StyledStr;
use clap::{Arg, ArgAction, Command};
use unicode_width::UnicodeWidthStr;

use crate::theme::{Role, Theme};

/// clap's own text for its `help` subcommand (`clap_builder` 4.6.7
/// `_check_help_and_version`), which sb keeps listed (maintainer).
const HELP_SUBCOMMAND_ABOUT: &str = "Print this message or the help of the given subcommand(s)";

/// The widest help sb lays out, fang's cap (fang v2.0.1 `help.go`,
/// `width`), and its width when neither the terminal nor `COLUMNS` gives
/// one.
pub const MAX_WIDTH: usize = 120;

/// The about's indent, fang's `PaddingLeft(2)`.
const ABOUT_INDENT: usize = 2;

/// The fewest columns a row's text wraps at. A terminal so narrow that the
/// help column leaves fewer wraps the text at this, past the edge, rather
/// than one or two characters a row.
const MIN_TEXT_WIDTH: usize = 20;

/// After a command's "Help for <name>" when it has a long help, which
/// `--help` shows and `-h` doesn't (the maintainer, 2026-11-15; fang has
/// one help text, so sb-go has no such hint).
const LONG_HELP_HINT: &str = " (--help shows more details)";

/// The width help is laid out for: stdout's terminal width, else
/// `COLUMNS`, in clap's order (`clap_builder` 4.6.7
/// `src/output/help_template.rs`, `dimensions`: the terminal first, the
/// variable when there is none), else [`MAX_WIDTH`]; never more than
/// [`MAX_WIDTH`], as fang. The terminal's width is `TIOCGWINSZ` through
/// rustix's `tcgetwinsize`
/// (<https://docs.rs/rustix/1.1.6/rustix/termios/fn.tcgetwinsize.html>),
/// as sb-exec reads it for a PTY.
#[must_use]
pub fn width() -> usize {
    width_of(std::io::stdout())
}

/// [`width`]'s rule for the terminal on `fd`.
fn width_of(fd: impl rustix::fd::AsFd) -> usize {
    let terminal = rustix::termios::tcgetwinsize(fd)
        .ok()
        .map(|size| usize::from(size.ws_col));
    layout_width(terminal, std::env::var_os("COLUMNS").as_deref())
}

/// [`width`]'s rule over what it read: a terminal width of 0 (a PTY
/// nobody sized) and a `COLUMNS` that isn't a positive number count as
/// none.
fn layout_width(terminal: Option<usize>, columns: Option<&std::ffi::OsStr>) -> usize {
    terminal
        .filter(|&w| w > 0)
        .or_else(|| {
            columns
                .and_then(|c| c.to_str()?.trim().parse::<usize>().ok())
                .filter(|&w| w > 0)
        })
        .map_or(MAX_WIDTH, |w| w.min(MAX_WIDTH))
}

/// One line of text wrapped at words to `width` display columns: a
/// line that fits comes back as it is; a longer one goes through tabled's
/// `Wrap::wrap`
/// (<https://docs.rs/tabled/0.22.0/tabled/settings/width/struct.Wrap.html#method.wrap>),
/// the wrap term-ui's tree uses (`sb_term_ui`'s `text::wrap_line`), and a
/// word wider than `width` (a long path or URL) breaks at any character,
/// from where it starts. The spaces at each break are taken off (tabled
/// carries the one it broke at to the next piece, which also makes the
/// line after an exactly full one break a column early) and a piece of
/// spaces only is dropped; the first piece keeps the line's own indent,
/// and a line of spaces only comes back as one empty piece. A line may
/// carry colour codes (a parse error's): tabled's `ansi` feature keeps a
/// colour across a break, and they count no columns.
///
/// Why not textwrap (`Options::subsequent_indent`,
/// <https://docs.rs/textwrap/0.16.2/textwrap/struct.Options.html#structfield.subsequent_indent>):
/// it isn't a dependency (a new crate goes to the maintainer), and
/// tabled's wrap is already in sb's binary. Help texts hold no colour
/// codes (the styles are added to each piece after); a parse error's
/// lines do, which tabled's `ansi` wrap carries across a break.
fn wrapped(line: &str, width: usize) -> Vec<String> {
    // Measured as tabled measures it: colour codes take no columns.
    if tabled::grid::util::string::get_line_width(line) <= width {
        return vec![line.to_owned()];
    }
    let wrapped = tabled::settings::width::Wrap::wrap(line, width, true);
    let pieces: Vec<&str> = wrapped
        .split('\n')
        .filter(|piece| tabled::grid::util::string::get_line_width(piece.trim()) > 0)
        .collect();
    if pieces.is_empty() {
        return vec![String::new()];
    }
    let last = pieces.len().saturating_sub(1);
    pieces
        .iter()
        .enumerate()
        .map(|(i, piece)| {
            let piece = if i > 0 {
                piece.trim_start_matches(' ')
            } else {
                piece
            };
            let piece = if i < last {
                piece.trim_end_matches(' ')
            } else {
                piece
            };
            piece.to_owned()
        })
        .collect()
}

/// Each command in `cmd` with its help replaced by sb's format. `colour`
/// says whether help will be shown in colour (the shaded usage block's
/// padding rows are left out otherwise, as fang does); `width` is the
/// width its lines wrap at ([`width`]).
///
/// Why not read the tree after `Command::build()`, which fills in command
/// paths, inherited globals and clap's help flag and subcommand: a built
/// command lists the help flag and every inherited global among its own
/// arguments, so fang's `[--flags]` rule (own flags only) couldn't tell them
/// apart without comparing each command with its parent, which costs as
/// much as passing the inherited flags down here. Only a command without
/// subcommands is read from a copy built alone, which inherits nothing,
/// for its deferred arguments ([`built`]).
#[must_use]
pub fn apply(cmd: Command, theme: &Theme, colour: bool, width: usize) -> Command {
    let path = cmd.get_name().to_string();
    let look = Look {
        theme,
        colour,
        width,
    };
    apply_at(cmd, &path, &[], &look, None)
}

/// [`apply`] for the root and the commands on `shown` alone, outermost
/// first (an alias is followed, as clap's `help` follows it): a line that shows one
/// command's help needs only that command's layout and its parents' (the
/// root's at least), not every command's (about half of `sb --help`'s
/// instructions: performance research, round 3, core item 5). Every
/// other command keeps clap's own help, so the caller must know whose
/// help clap shows (cli-core's `colour::Early::path`); a name that isn't
/// a subcommand ends the walk.
#[must_use]
pub fn apply_to(
    cmd: Command,
    theme: &Theme,
    colour: bool,
    width: usize,
    shown: &[String],
) -> Command {
    let path = cmd.get_name().to_string();
    let look = Look {
        theme,
        colour,
        width,
    };
    apply_at(cmd, &path, &[], &look, Some(shown))
}

/// How help is drawn: the theme, whether in colour, and the width its
/// lines wrap at.
struct Look<'a> {
    theme: &'a Theme,
    colour: bool,
    width: usize,
}

/// What goes before a rendered text: fang's one blank line above the
/// about (the maintainer, 2026-11-14), and one more for clap to trim.
/// clap's `write_help` drops the first line when it is blank, one line
/// only (`clap_builder` 4.6.7 `src/output/help.rs`, `write_help`, and
/// `StyledStr::trim_start_lines`); sb's help tests check that one blank
/// line is left.
const LEAD: &str = "\n\n";

/// `cmd`'s help laid out at `path`, then each subcommand's, or only the
/// next one `shown` names (and its own, down the line) when given.
fn apply_at(
    cmd: Command,
    path: &str,
    inherited: &[Arg],
    look: &Look<'_>,
    shown: Option<&[String]>,
) -> Command {
    let refs: Vec<&Arg> = inherited.iter().collect();
    let (text, long, below) = {
        let view = built(&cmd);
        // A command with no about starts at its USAGE heading's blank line:
        // still one blank line, as fang's.
        let led = |long| {
            let text = render(&view, path, &refs, look, long);
            format!("{LEAD}{}", text.trim_start_matches('\n'))
        };
        let mut below = inherited.to_vec();
        below.extend(own_arguments(&view).filter(|a| a.is_global_set()).cloned());
        (
            led(false),
            has_long_help(&view, &refs).then(|| led(true)),
            below,
        )
    };
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    let mut cmd = cmd
        .help_template("{before-help}")
        .before_help(StyledStr::from(text));
    if let Some(long) = long {
        cmd = cmd.before_long_help(StyledStr::from(long));
    }
    // `mut_subcommand` moves the command it changes to the end (clap_builder
    // 4.6.7 `Command::mut_subcommand`), so walking them in order keeps
    // their order: sb's parse renders errors (`[subcommands: …]`, its
    // suggestions) from the tree without this layout, and they must read
    // the same. Changing one alone moves it to the end: only a line that
    // shows help does that, and help is rendered before the move.
    if let Some(shown) = shown {
        // By its own name: `sb help` takes an alias too, as clap's does.
        let next = shown.split_first().and_then(|(next, rest)| {
            cmd.find_subcommand(next)
                .map(|sub| (sub.get_name().to_owned(), rest))
        });
        if let Some((name, rest)) = next {
            let sub_path = format!("{path} {name}");
            cmd = cmd.mut_subcommand(&name, |sub| {
                apply_at(sub, &sub_path, &below, look, Some(rest))
            });
        }
        return cmd;
    }
    for name in names {
        let sub_path = format!("{path} {name}");
        cmd = cmd.mut_subcommand(&name, |sub| apply_at(sub, &sub_path, &below, look, None));
    }
    cmd
}

/// `cmd` as clap builds it, to read its arguments from: a command whose
/// arguments clap's derive defers (`#[command(defer = true)]`) has none
/// until clap builds it, and clap's docs say to call `Command::build`
/// before inspecting deferred arguments
/// (<https://docs.rs/clap/4.6.7/clap/_derive/index.html#command-attributes>).
/// A copy is built, alone, and only for a command without subcommands:
/// the tree itself stays unbuilt for the parse, which builds the commands
/// a line selects in their place (a command built alone gets none of its
/// parent's settings or global flags), and the derive builds a deferred
/// command's subcommands with its arguments, so a command that has
/// subcommands isn't deferred (sb's groups are enums of their own,
/// flattened, which keep their own `defer`). Built alone, a copy lists
/// clap's help flag among its arguments ([`own_arguments`] leaves it
/// out) and no inherited flag. A built command lays out as the same
/// command unbuilt: help reads no setting the build changes (it sets
/// `false`'s and `0`'s defaults on switches and counts, which a flag's
/// row doesn't show; it would set `true` on an `ArgAction::SetFalse`
/// flag, which sb has none of). In a debug build, clap's checks on the
/// copy would refuse an argument that names an inherited one
/// (`conflicts_with` a parent's global): none does.
fn built(cmd: &Command) -> Cow<'_, Command> {
    if cmd.has_subcommands() {
        return Cow::Borrowed(cmd);
    }
    let mut copy = cmd.clone();
    copy.build();
    Cow::Owned(copy)
}

/// `cmd`'s own arguments: clap's help flag, which a built command lists
/// (`clap_builder` 4.6.7 `Command::_check_help_and_version`), left out.
/// Why not `disable_help_flag` on the copy: [`render`] reads that setting
/// to draw the `-h --help` row.
fn own_arguments(cmd: &Command) -> impl Iterator<Item = &Arg> {
    cmd.get_arguments()
        .filter(|a| !(a.get_id() == "help" && matches!(a.get_action(), ArgAction::Help)))
}

/// Whether `cmd` has a long text that `--help` shows and `-h` doesn't: a
/// shown flag's long help, or the command's own `after_long_help` (its
/// examples, `sb install`'s).
fn has_long_help(cmd: &Command, inherited: &[&Arg]) -> bool {
    cmd.get_after_long_help().is_some()
        || shown_flags(cmd, inherited).any(|a| a.get_long_help().is_some())
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
            let _ = write!(hint, " <{name}>");
        } else {
            let _ = write!(hint, " [{name}]");
        }
    }
    if own_arguments(cmd).any(|a| !a.is_positional() && !a.is_hide_set()) {
        hint.push_str(" [--flags]");
    }
    hint
}

/// The flags `cmd`'s help lists: its own and `inherited`, positionals and
/// hidden ones left out.
fn shown_flags<'a>(cmd: &'a Command, inherited: &'a [&'a Arg]) -> impl Iterator<Item = &'a Arg> {
    own_arguments(cmd)
        .chain(inherited.iter().copied())
        .filter(|a| !a.is_positional() && !a.is_hide_set())
}

/// A flag as fang shows it: `-s --long` (no value placeholder), its help
/// (the long one, if it has one, when `long_help`), and its default as
/// ` (value)` unless empty, false, 0 or [].
fn flag_row(arg: &Arg, long_help: bool) -> (String, String, String) {
    let long = arg.get_long().unwrap_or_default().to_string();
    let shown = match (arg.get_short(), arg.get_long()) {
        (Some(short), Some(long)) => format!("-{short} --{long}"),
        (Some(short), None) => format!("-{short}"),
        (None, _) => format!("--{long}"),
    };
    let help = if long_help {
        arg.get_long_help().or(arg.get_help())
    } else {
        arg.get_help()
    };
    let mut help = help.map_or_default(ToString::to_string);
    let default = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy())
        .collect::<Vec<_>>()
        .join(",");
    if !matches!(default.as_str(), "" | "false" | "0" | "[]") {
        let _ = write!(help, " ({default})");
    }
    let key = if long.is_empty() { shown.clone() } else { long };
    (key, shown, help)
}

/// `help` beside a row's `lead`, which ends at the help column `column`,
/// in `style`: each of its lines wrapped to `width`, every piece after the
/// first under the help column. A blank line in `help` (a long help's
/// paragraph break, as `-e`'s) is left out, so the command and flag lists
/// have no blank line (maintainer, 2026-11-14); the row's first line is
/// written even when empty, for its lead.
fn write_row(out: &mut String, lead: &str, help: &str, column: usize, width: usize, style: Style) {
    let avail = width.saturating_sub(column).max(MIN_TEXT_WIDTH);
    let indent = " ".repeat(column);
    let mut first = true;
    for line in help.split('\n') {
        if line.trim().is_empty() && !first {
            continue;
        }
        let (head, body) = hang(line, avail);
        let pad = " ".repeat(head.width());
        for (i, piece) in wrapped(body, avail - head.width()).iter().enumerate() {
            let lead = if first { lead } else { indent.as_str() };
            let start = if i == 0 { head } else { pad.as_str() };
            let _ = writeln!(out, "{lead}{style}{start}{piece}{style:#}");
            first = false;
        }
    }
}

/// `line` split where its later pieces hang when it doesn't fit in
/// `avail` columns, the first of these that leaves at least
/// [`MIN_TEXT_WIDTH`] columns: for an indented line, after its first gap
/// of two or more spaces past the indent (an example's command, then its
/// note: the command stays whole), then after its indent; else not at
/// all (the help column). A line that fits isn't
/// split. Custom: tabled's `Wrap` has no hanging indent, and textwrap's
/// `subsequent_indent` isn't a dependency (the maintainer kept tabled,
/// 2026-11-14).
fn hang(line: &str, avail: usize) -> (&str, &str) {
    if line.width() <= avail {
        return ("", line);
    }
    let text = line.trim_start_matches(' ');
    let indent = line.len() - text.len();
    // Only an indented line is an example; a gap in a sentence isn't.
    let note = (indent > 0)
        .then(|| text.find("  "))
        .flatten()
        .map(|gap| line.len() - text[gap..].trim_start_matches(' ').len());
    note.into_iter()
        .chain([indent])
        .map(|at| line.split_at(at))
        .find(|(head, _)| avail.saturating_sub(head.width()) >= MIN_TEXT_WIDTH)
        .unwrap_or(("", line))
}

/// The help text for `cmd`, reached through `path` (`sb install`), with
/// `inherited` the global flags of its ancestors; its flags' long help
/// when `long`.
fn render(cmd: &Command, path: &str, inherited: &[&Arg], look: &Look<'_>, long: bool) -> String {
    let Look {
        theme,
        colour,
        width,
    } = *look;
    let mut commands: Vec<(String, String, String)> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| {
            let about = c.get_about().map_or_default(ToString::to_string);
            (c.get_name().to_string(), hint(&built(c)), about)
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
    let mut flags: Vec<(String, String, String)> = shown_flags(cmd, inherited)
        .map(|arg| flag_row(arg, long))
        .collect();
    if !cmd.is_disable_help_flag_set() {
        // A command with a long help says so on `-h` and `--help` alike.
        let more = if has_long_help(cmd, inherited) {
            LONG_HELP_HINT
        } else {
            ""
        };
        flags.push((
            "help".into(),
            "-h --help".into(),
            format!("Help for {}{more}", cmd.get_name()),
        ));
    }
    flags.sort();
    // The key column: the widest key plus two (fang's `calculateSpace`).
    let key_width = commands
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

    // fang shows the long description, else the short one; `--help`
    // adds the command's `after_long_help` below it, as a paragraph of
    // the about (clap's own place for it is after the flags, but sb's
    // template draws only `before-help`).
    let mut about = cmd
        .get_long_about()
        .or(cmd.get_about())
        .map_or_default(ToString::to_string);
    if long && let Some(after) = cmd.get_after_long_help() {
        about = format!("{about}\n\n{after}");
    }
    let mut out = String::new();
    // fang wraps the about at the width less its indent.
    let about_width = width.saturating_sub(ABOUT_INDENT).max(MIN_TEXT_WIDTH);
    let indent = " ".repeat(ABOUT_INDENT);
    for line in about.lines() {
        if line.is_empty() {
            out.push('\n');
            continue;
        }
        for piece in wrapped(line, about_width) {
            let _ = writeln!(out, "{indent}{text}{piece}{text:#}");
        }
    }
    let _ = writeln!(out, "\n  {heading}USAGE{heading:#}\n");
    out.push_str(&usage_block(cmd, path, theme, colour));
    if !commands.is_empty() {
        let _ = writeln!(out, "\n  {heading}COMMANDS{heading:#}\n");
        for (name, hint, about) in &commands {
            let pad = " ".repeat(key_width - name.width() - hint.width());
            let lead = format!("    {name_style}{name}{name_style:#}{muted}{hint}{muted:#}{pad}");
            write_row(&mut out, &lead, about, key_width + 4, width, text);
        }
    }
    let _ = writeln!(out, "\n  {heading}FLAGS{heading:#}\n");
    for (_, shown, help) in &flags {
        let pad = " ".repeat(key_width - shown.width());
        let lead = format!("    {flag}{shown}{flag:#}{pad}");
        write_row(&mut out, &lead, help, key_width + 4, width, text);
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
/// (maintainer, 2026-11-01, look H1 in proto-help): an `ERROR` badge on the
/// problem colour, a blank line, clap's message with the value you typed in
/// the problem colour (taken from clap's error context, so it is exact),
/// then clap's tips as `hint:` lines (see [`tip_hint`]; the label in the
/// warn colour) and clap's context lines (`[possible values: …]`, the
/// values in the OK colour).
/// Every line wraps at words to `width` (the stderr terminal's,
/// [`error_width`]), its later rows under its two-column indent, through
/// the help's own wrap ([`wrapped`], tabled's `Wrap::wrap`, which keeps a
/// colour across a break with its `ansi` feature).
/// No pointer to help, as sb-go. clap's usage paragraph is left out:
/// removed with clap's `Error::remove(ContextKind::Usage)`
/// (<https://docs.rs/clap/4.6.7/clap/error/struct.Error.html#method.remove>),
/// and skipped by line for errors whose usage is part of their message
/// (`Error::format`ted ones, which sb doesn't produce today).
///
/// Why not a clap `ErrorFormatter`
/// (<https://docs.rs/clap/4.6.7/clap/error/trait.ErrorFormatter.html>): clap
/// builds its messages in crate-private code (`write_dynamic_context`,
/// `did_you_mean`, `try_help` in `clap_builder` 4.6.7 `error/format.rs`), so a
/// formatter of ours would have to rewrite every message, and clap skips it
/// for errors that carry their own message. This keeps clap's wording and
/// re-lays it out from its plain rendering (`StyledStr`'s Display drops
/// styles). It keys on clap's English (`error: `, `tip: `, `Usage:`, `For
/// more information`, `[possible values: `, `a similar `, `some similar `,
/// `, use '`); the tests pin each shape, so a clap update that changes one
/// fails them.
#[must_use]
pub fn parse_error_block(mut err: clap::Error, theme: &Theme, width: usize) -> String {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    let _ = err.remove(ContextKind::Usage);
    // The value the user typed, exactly as clap recorded it (so an
    // apostrophe in it can't shift what gets coloured).
    let typed = match err.kind() {
        ErrorKind::InvalidSubcommand => err.get(ContextKind::InvalidSubcommand),
        ErrorKind::UnknownArgument => err.get(ContextKind::InvalidArg),
        ErrorKind::InvalidValue | ErrorKind::ValueValidation | ErrorKind::TooManyValues => {
            err.get(ContextKind::InvalidValue)
        }
        _ => None,
    }
    .and_then(|value| match value {
        ContextValue::String(value) if !value.is_empty() => Some(value.clone()),
        _ => None,
    });
    let text = err.render().to_string();
    let (problem, ok, warn) = (
        theme.style(Role::Problem),
        theme.style(Role::Ok),
        theme.style(Role::Warn),
    );
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let headline = first.strip_prefix("error: ").unwrap_or(first);
    let headline = match &typed {
        Some(value) => headline.replacen(
            &format!("'{value}'"),
            &format!("'{problem}{value}{problem:#}'"),
            1,
        ),
        None => headline.to_string(),
    };
    let mut body = Vec::new();
    let mut in_usage = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            in_usage = false;
        } else if trimmed.starts_with("Usage:") {
            in_usage = true;
        } else if in_usage || trimmed.starts_with("For more information, ") {
            // Usage lines and clap's closing pointer are left out.
        } else if let Some(tip) = trimmed.strip_prefix("tip: ") {
            body.push(format!("{warn}hint:{warn:#} {}", tip_hint(tip, ok)));
        } else if let Some(list) = trimmed
            .strip_prefix("[possible values: ")
            .and_then(|rest| rest.strip_suffix(']'))
        {
            let values: Vec<String> = list.split(", ").map(|v| format!("{ok}{v}{ok:#}")).collect();
            body.push(format!("[possible values: {}]", values.join(", ")));
        } else {
            body.push(trimmed.to_string());
        }
    }
    let badge = match theme.color(Role::Problem) {
        // The badge text takes the on-accent colour, the palette's dark text
        // for a bright background (the theme's `text` is light and wouldn't
        // read on the problem colour).
        Some(bg) => Style::new()
            .bg_color(Some(bg))
            .fg_color(theme.color(Role::OnAccent))
            .bold(),
        None => Style::new(),
    };
    let mut out = format!("\n  {badge} ERROR {badge:#}\n\n");
    let indented = |out: &mut String, line: &str| {
        for piece in wrapped(line, width.saturating_sub(ERROR_INDENT).max(MIN_TEXT_WIDTH)) {
            let _ = writeln!(out, "  {piece}");
        }
    };
    indented(&mut out, &headline);
    if !body.is_empty() {
        out.push('\n');
    }
    for line in body {
        indented(&mut out, &line);
    }
    out.push('\n');
    out
}

/// A parse error's lines' indent (fang's).
const ERROR_INDENT: usize = 2;

/// The width parse errors wrap at: [`width`]'s rule, for stderr's
/// terminal, where they are printed.
#[must_use]
pub fn error_width() -> usize {
    width_of(std::io::stderr())
}

/// One of clap's tips as a hint. clap's suggestions ("a similar subcommand
/// exists: 'x'", "some similar subcommands exist: 'x', 'y'") read as "did
/// you mean 'x' or 'y'?", each name in `ok`, so no line has two colons; a
/// tip that names a fix ("…, use '-- --x'") has it in `ok`; others stay
/// clap's text.
fn tip_hint(tip: &str, ok: Style) -> String {
    let suggested = tip
        .strip_prefix("a similar ")
        .or_else(|| tip.strip_prefix("some similar "))
        .and_then(|rest| rest.split_once(": '"))
        .and_then(|(_, names)| names.strip_suffix('\''));
    if let Some(names) = suggested {
        // clap lists them by ascending similarity, the most similar last
        // (`clap_builder` 4.6.7 `parser/features/suggestions.rs`,
        // `did_you_mean`): the best guess goes first.
        let mut names: Vec<String> = names
            .split("', '")
            .map(|name| format!("'{ok}{name}{ok:#}'"))
            .collect();
        names.reverse();
        let names = match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
            _ => names.concat(),
        };
        return format!("did you mean {names}?");
    }
    match tip
        .rsplit_once(", use '")
        .and_then(|(before, fix)| Some((before, fix.strip_suffix('\'')?)))
    {
        Some((before, fix)) => format!("{before}, use '{ok}{fix}{ok:#}'"),
        None => tip.to_string(),
    }
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
            .subcommand(version_args(
                Command::new("version").about("Print Saltbox CLI version"),
            ))
            .subcommand(install_args(
                Command::new("install")
                    .about("Runs Ansible playbooks with specified tags")
                    .long_about("Runs Ansible playbooks.\n\nTags select what runs."),
            ))
            .subcommand(Command::new("secret").about("Hidden").hide(true))
    }

    fn version_args(version: Command) -> Command {
        version.arg(
            Arg::new("json")
                .long("json")
                .action(ArgAction::SetTrue)
                .help("Print machine-readable version information"),
        )
    }

    fn install_args(install: Command) -> Command {
        install
            .arg(Arg::new("tags").value_name("tags").num_args(0..))
            .arg(
                Arg::new("skip-tags")
                    .short('s')
                    .long("skip-tags")
                    .help("Tags to skip"),
            )
    }

    /// [`sb`] with `version`'s and `install`'s arguments deferred until
    /// clap builds them (`Command::defer`, what clap's derive does with
    /// `#[command(defer = true)]`).
    fn deferred() -> Command {
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
                    .defer(version_args),
            )
            .subcommand(
                Command::new("install")
                    .about("Runs Ansible playbooks with specified tags")
                    .long_about("Runs Ansible playbooks.\n\nTags select what runs.")
                    .defer(install_args),
            )
            .subcommand(Command::new("secret").about("Hidden").hide(true))
    }

    /// A command whose arguments are deferred shows the help it would show
    /// with them built at once: the parent's hints and the command's own
    /// flags, through `apply` and `apply_to`, for `-h`, `--help` and `help`.
    #[test]
    fn deferred_arguments_read_as_built_ones() {
        let lines: &[&[&str]] = &[
            &["sb", "--help"],
            &["sb", "-h"],
            &["sb", "install", "--help"],
            &["sb", "install", "-h"],
            &["sb", "help", "install"],
            &["sb", "version", "--help"],
        ];
        for line in lines {
            assert_eq!(shown(deferred(), line), shown(sb(), line), "{line:?}");
            let shown_to = |cmd: Command| {
                let path: Vec<String> = line[1..]
                    .iter()
                    .filter(|word| !word.starts_with('-') && **word != "help")
                    .map(ToString::to_string)
                    .collect();
                apply_to(cmd, &Theme::plain(), false, MAX_WIDTH, &path)
                    .try_get_matches_from(*line)
                    .expect_err("help is shown as an error")
                    .to_string()
            };
            assert_eq!(shown_to(deferred()), shown_to(sb()), "{line:?}");
        }
    }

    fn look(theme: &Theme, colour: bool, width: usize) -> Look<'_> {
        Look {
            theme,
            colour,
            width,
        }
    }

    fn plain(cmd: &Command, path: &str, inherited: &[&Arg]) -> String {
        render(
            cmd,
            path,
            inherited,
            &look(&Theme::plain(), false, MAX_WIDTH),
            false,
        )
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
        let text = render(&sb(), "sb", &[], &look(&theme, true, MAX_WIDTH), false);
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
        let mut cmd = apply(sb(), &Theme::plain(), false, MAX_WIDTH);
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
        let via_help = apply(sb(), &Theme::plain(), false, MAX_WIDTH)
            .try_get_matches_from(["sb", "help", "version"])
            .expect_err("help is shown as an error")
            .to_string();
        assert!(
            via_help.contains("    sb version [--flags]\n"),
            "{via_help}"
        );
    }

    /// A command with a flag that has a long help (clap's `Arg::long_help`,
    /// <https://docs.rs/clap/4.6.7/clap/struct.Arg.html#method.long_help>).
    fn with_long_help() -> Command {
        sb().mut_subcommand("install", |install| {
            install.arg(
                Arg::new("extra-vars")
                    .short('e')
                    .long("extra-vars")
                    .help("Extra variables")
                    .long_help("Extra variables\n\nEach value reaches Ansible whole."),
            )
        })
    }

    /// The help clap prints for `args` once `apply` has run.
    fn shown(cmd: Command, args: &[&str]) -> String {
        apply(cmd, &Theme::plain(), false, MAX_WIDTH)
            .try_get_matches_from(args)
            .expect_err("help is shown as an error")
            .to_string()
    }

    /// A command's `after_long_help` (examples) follows its about on
    /// `--help` only, a blank line between, under the about's indent; its
    /// `-h --help` row says `--help` shows more, on both.
    #[test]
    fn a_commands_examples_show_on_help_only() {
        let cmd = || {
            Command::new("sb").subcommand(
                Command::new("install")
                    .about("Runs playbooks")
                    .after_long_help("Examples:\n  sb install plex  # One tag"),
            )
        };
        let long = shown(cmd(), &["sb", "install", "--help"]);
        assert!(
            long.contains(
                "  Runs playbooks\n\n  Examples:\n    sb install plex  # One tag\n\n  USAGE"
            ),
            "{long}"
        );
        assert!(
            long.contains("Help for install (--help shows more details)"),
            "{long}"
        );
        let short = shown(cmd(), &["sb", "install", "-h"]);
        assert!(!short.contains("Examples:"), "{short}");
        assert!(
            short.contains("Help for install (--help shows more details)"),
            "{short}"
        );
    }

    /// `--help` shows a flag's long help, its later lines under the help
    /// column and its blank line left out (maintainer, 2026-11-14); `-h`
    /// shows the short help; `help <command>` is clap's long help, as
    /// `--help`.
    #[test]
    fn long_help_shows_on_help_only() {
        let long = shown(with_long_help(), &["sb", "install", "--help"]);
        assert!(
            long.contains("    -e --extra-vars  Extra variables\n                     Each value reaches Ansible whole.\n"),
            "{long}"
        );
        let short = shown(with_long_help(), &["sb", "install", "-h"]);
        assert!(
            short.contains("    -e --extra-vars  Extra variables\n"),
            "{short}"
        );
        assert!(!short.contains("Each value"), "{short}");
        assert_eq!(
            shown(with_long_help(), &["sb", "help", "install"]),
            long,
            "help <command> is the long help"
        );
        // The rest is the same text.
        assert_eq!(
            long.replace(
                "\n                     Each value reaches Ansible whole.",
                ""
            ),
            short
        );
    }

    /// A command with a long help hints at it in its help row, in `-h`
    /// and `--help` alike (the maintainer, 2026-11-15); one without keeps
    /// sb-go's row.
    #[test]
    fn the_help_row_hints_at_the_long_help() {
        let hint = "    -h --help        Help for install (--help shows more details)\n";
        for args in [["sb", "install", "-h"], ["sb", "install", "--help"]] {
            let help = shown(with_long_help(), &args);
            assert!(help.contains(hint), "{args:?}: {help}");
        }
        let root = shown(with_long_help(), &["sb", "--help"]);
        assert!(root.contains("Help for sb\n"), "{root}");
        assert!(!root.contains("shows more"), "{root}");
        let plain = shown(sb(), &["sb", "install", "--help"]);
        assert!(plain.contains("Help for install\n"), "{plain}");
    }

    /// A row's indented line wraps under its own indent, and one with a
    /// note after a gap of spaces under the note, so an example's command
    /// stays whole; a line that fits is untouched, and one whose hang
    /// would leave too few columns wraps under the help column.
    #[test]
    fn an_indented_line_hangs_under_its_indent_or_note() {
        let rows = |help: &str, width: usize| {
            let mut out = String::new();
            write_row(&mut out, "    -e  ", help, 8, width, Style::new());
            out
        };
        let help = "Intro\n  -e one two three four five six seven eight nine ten\n  -e @f.yml   a file relative to the repository it runs in\n  -e fits";
        assert_eq!(
            rows(help, 48),
            concat!(
                "    -e  Intro\n",
                "          -e one two three four five six seven\n",
                "          eight nine ten\n",
                "          -e @f.yml   a file relative to the\n",
                "                      repository it runs in\n",
                "          -e fits\n",
            )
        );
        // 30 columns: the note's hang leaves fewer than 20, the indent's
        // more.
        assert_eq!(
            rows("  -e @f.yml   a file relative to the repository", 38),
            concat!(
                "    -e    -e @f.yml   a file relative\n",
                "          to the repository\n",
            )
        );
        // An unindented line with a gap (two spaces between sentences)
        // isn't an example: it wraps under the help column.
        assert_eq!(
            rows(
                "One sentence ends.  Another one follows it here and more",
                48
            ),
            concat!(
                "    -e  One sentence ends.  Another one follows\n",
                "        it here and more\n",
            )
        );
        // 21 columns: neither hang leaves 20; the help column's text.
        assert_eq!(
            rows("  -e @f.yml   a file relative to the repository", 29),
            concat!(
                "    -e    -e @f.yml   a file\n",
                "        relative to the\n",
                "        repository\n",
            )
        );
    }

    /// A command with no long help shows one text for `-h`, `--help` and
    /// `help <command>`, as before; so does a command whose only long
    /// help is in another command.
    #[test]
    fn without_a_long_help_both_flags_show_one_text() {
        for (cmd, path) in [(sb(), "version"), (with_long_help(), "version")] {
            let long = shown(cmd.clone(), &["sb", path, "--help"]);
            assert_eq!(shown(cmd.clone(), &["sb", path, "-h"]), long);
            assert_eq!(shown(cmd, &["sb", "help", path]), long);
            assert!(long.contains("    sb version [--flags]\n"), "{long}");
        }
        let root = shown(with_long_help(), &["sb", "--help"]);
        assert_eq!(shown(with_long_help(), &["sb", "-h"]), root);
    }

    /// An inherited flag's long help shows in the subcommand's long help.
    #[test]
    fn an_inherited_flags_long_help_shows_below() {
        let cmd = sb().mut_arg("color", |color| {
            color.long_help("Controls when to use color\n\nauto, always or never.")
        });
        let long = shown(cmd.clone(), &["sb", "version", "--help"]);
        assert!(long.contains("auto, always or never."), "{long}");
        assert!(!shown(cmd, &["sb", "version", "-h"]).contains("auto, always"));
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

    /// clap's derive names positionals in `SCREAMING_SNAKE` (`TAGS`); sb-go's
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
        let mut cmd = apply(cmd, &Theme::plain(), false, MAX_WIDTH);
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

    /// clap's usage paragraph, every line of it, and its pointer to help are
    /// left out; tips become `hint:` lines; context lines stay.
    #[test]
    fn a_parse_error_keeps_claps_message_without_its_usage() {
        let err = clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            "invalid value 'x'\n  [possible values: a, b]\n\n  tip: try 'a'\n\nUsage: sb one\n       sb two\n\nFor more information, try '--help'.\n",
        );
        assert_eq!(
            parse_error_block(err, &Theme::plain(), MAX_WIDTH),
            "\n   ERROR \n\n  invalid value 'x'\n\n  [possible values: a, b]\n  hint: try 'a'\n\n"
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
        let block = parse_error_block(err, &theme, MAX_WIDTH);
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
            block.contains(&format!("'{}nope", theme.style(Role::Problem))),
            "{block:?}"
        );
        let typo = Command::new("sb")
            .subcommand(Command::new("version"))
            .try_get_matches_from(["sb", "verison"])
            .expect_err("typo");
        let block = parse_error_block(typo, &theme, MAX_WIDTH);
        assert!(
            block.contains(&format!("{}hint:", theme.style(Role::Warn)))
                && block.contains(&format!("'{}version", theme.style(Role::Ok))),
            "{block:?}"
        );
    }

    fn plain_error(cmd: Command, argv: &[&str]) -> String {
        parse_error_block(
            cmd.try_get_matches_from(argv).expect_err("a parse error"),
            &Theme::plain(),
            MAX_WIDTH,
        )
    }

    /// clap's real output for the kinds with extra lines, so a clap update
    /// that changes their shape fails here: missing arguments, a conflict
    /// with several arguments, a missing subcommand and a multi-line usage.
    #[test]
    fn claps_multi_line_errors_keep_their_lines() {
        let add = Command::new("sb").subcommand(
            Command::new("add")
                .arg(Arg::new("name").required(true))
                .arg(Arg::new("path").required(true)),
        );
        assert_eq!(
            plain_error(add, &["sb", "add"]),
            "\n   ERROR \n\n  the following required arguments were not provided:\n\n  <name>\n  <path>\n\n"
        );
        let flag = |id: &'static str| Arg::new(id).long(id).action(ArgAction::SetTrue);
        let conflict = Command::new("sb")
            .arg(flag("a"))
            .arg(flag("b"))
            .arg(flag("c").conflicts_with_all(["a", "b"]));
        assert_eq!(
            plain_error(conflict, &["sb", "--c", "--a", "--b"]),
            "\n   ERROR \n\n  the argument '--c' cannot be used with:\n\n  --a\n  --b\n\n"
        );
        let required = Command::new("sb")
            .subcommand_required(true)
            .arg(flag("verbose"))
            .subcommand(Command::new("version"));
        assert_eq!(
            plain_error(required, &["sb", "--verbose"]),
            "\n   ERROR \n\n  'sb' requires a subcommand but one was not provided\n\n  [subcommands: version, help]\n\n"
        );
        let two_usages = Command::new("sb")
            .args_conflicts_with_subcommands(true)
            .arg(Arg::new("x").long("x").required(true))
            .subcommand(Command::new("version"));
        assert_eq!(
            plain_error(two_usages, &["sb", "--bogus"]),
            "\n   ERROR \n\n  unexpected argument '--bogus' found\n\n"
        );
    }

    /// No pointer to help, here with neither clap's help flag nor its help
    /// subcommand.
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

    /// No pointer to help either way, as sb-go (here with clap's help
    /// subcommand on and its flag off).
    #[test]
    fn no_help_pointer_with_only_the_help_subcommand() {
        let cmd = Command::new("sb")
            .disable_help_flag(true)
            .subcommand(Command::new("version"));
        assert_eq!(
            plain_error(cmd, &["sb", "--bogus"]),
            "\n   ERROR \n\n  unexpected argument '--bogus' found\n\n"
        );
    }

    /// Possible values are green; an error whose first quoted value isn't
    /// what the user typed ('sb' requires a subcommand) leaves it uncoloured.
    #[test]
    fn values_are_green_and_only_typed_values_are_marked() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let (ok, problem) = (theme.style(Role::Ok), theme.style(Role::Problem));
        let bad = Command::new("sb")
            .arg(Arg::new("mode").long("mode").value_parser(["a", "b"]))
            .try_get_matches_from(["sb", "--mode=c"])
            .expect_err("bad value");
        let block = parse_error_block(bad, &theme, MAX_WIDTH);
        assert!(
            block.contains(&format!("'{problem}c{problem:#}'")),
            "{block:?}"
        );
        assert!(
            block.contains(&format!("[possible values: {ok}a{ok:#}, {ok}b{ok:#}]")),
            "{block:?}"
        );
        let missing = Command::new("sb")
            .subcommand_required(true)
            .arg(
                Arg::new("verbose")
                    .long("verbose")
                    .action(ArgAction::SetTrue),
            )
            .subcommand(Command::new("version"))
            .try_get_matches_from(["sb", "--verbose"])
            .expect_err("missing subcommand");
        let block = parse_error_block(missing, &theme, MAX_WIDTH);
        assert!(block.contains("  'sb' requires a subcommand"), "{block:?}");
    }

    /// Several similar names read as one question, each candidate green,
    /// the best guess first: clap lists them by ascending similarity,
    /// most similar last.
    #[test]
    fn several_suggestions_read_as_one_question() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let ok = theme.style(Role::Ok);
        let err = Command::new("sb")
            .subcommand(Command::new("list"))
            .subcommand(Command::new("install"))
            .try_get_matches_from(["sb", "instal"])
            .expect_err("typo");
        let block = parse_error_block(err, &theme, MAX_WIDTH);
        assert!(
            block.contains(&format!(
                "did you mean '{ok}install{ok:#}' or '{ok}list{ok:#}'?"
            )),
            "{block:?}"
        );
        assert!(!block.contains("exist:"), "{block:?}");
    }

    /// At a narrow width every line wraps at words under its two-column
    /// indent, colours kept; a line that fits is left whole.
    #[test]
    fn a_parse_error_wraps_under_its_indent() {
        let commands = ["reinstall-python", "reinstall-facts", "reinstall-venv"];
        let err = || {
            commands
                .iter()
                .fold(Command::new("sb"), |cmd, name| {
                    cmd.subcommand(Command::new(*name))
                })
                .try_get_matches_from(["sb", "reinstall"])
                .expect_err("typo")
        };
        let block = parse_error_block(err(), &Theme::plain(), 40);
        let lines: Vec<&str> = block.lines().collect();
        assert!(lines.iter().all(|line| line.width() <= 40), "{block}");
        let hint = lines
            .iter()
            .position(|line| line.starts_with("  hint: did you mean"))
            .expect("the hint");
        let next = lines[hint + 1];
        assert!(
            next.starts_with("  ") && !next.starts_with("   "),
            "{block}"
        );
        assert_eq!(block.matches("reinstall-").count(), 3, "{block}");
        // Coloured, the codes don't count towards the width.
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let coloured = parse_error_block(err(), &theme, 40);
        let plain = anstream::adapter::strip_str(&coloured).to_string();
        assert!(plain.lines().all(|line| line.width() <= 40), "{plain}");
        assert_eq!(plain, block);
        // Wide, it is one line.
        let wide = parse_error_block(err(), &Theme::plain(), MAX_WIDTH);
        assert!(
            wide.lines()
                .any(|line| line.starts_with("  hint: did you mean 'reinstall-")
                    && line.ends_with('?')),
            "{wide}"
        );
    }

    /// The typed value is coloured whole, apostrophes and all, and a tip's
    /// fix only where the tip names one ("use '…'").
    #[test]
    fn typed_values_with_apostrophes_are_coloured_whole() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let (ok, problem) = (theme.style(Role::Ok), theme.style(Role::Problem));
        let err = Command::new("sb")
            .subcommand(Command::new("version"))
            .try_get_matches_from(["sb", "it's"])
            .expect_err("unknown command");
        let block = parse_error_block(err, &theme, MAX_WIDTH);
        assert!(
            block.contains(&format!("'{problem}it's{problem:#}'")),
            "{block:?}"
        );
        let err = Command::new("sb")
            .arg(Arg::new("name"))
            .try_get_matches_from(["sb", "--it's"])
            .expect_err("unknown flag");
        let block = parse_error_block(err, &theme, MAX_WIDTH);
        assert!(
            block.contains(&format!("'{problem}--it's{problem:#}'")),
            "{block:?}"
        );
        assert!(
            block.contains(&format!("use '{ok}-- --it's{ok:#}'")),
            "{block:?}"
        );
        let err = Command::new("sb")
            .subcommand(Command::new("version"))
            .try_get_matches_from(["sb", "--", "version"])
            .expect_err("subcommand after --");
        let block = parse_error_block(err, &theme, MAX_WIDTH);
        assert!(!block.contains(&format!("{ok}--{ok:#}")), "{block:?}");
    }

    /// An empty value ("a value is required for …") has nothing typed to
    /// colour, so the message stays plain.
    #[test]
    fn an_empty_value_colours_nothing() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let err = Command::new("sb")
            .arg(Arg::new("mode").long("mode").value_parser(["a", "b"]))
            .try_get_matches_from(["sb", "--mode="])
            .expect_err("empty value");
        let block = parse_error_block(err, &theme, MAX_WIDTH);
        let headline = block.lines().nth(3).expect("headline");
        assert!(!headline.contains('\x1b'), "{block:?}");
    }

    /// `width`'s rule: the terminal first, then `COLUMNS` (clap's order),
    /// else 120; never more than 120 (fang's cap).
    #[test]
    fn the_width_is_the_terminals_then_columns_capped_at_120() {
        use std::ffi::OsStr;
        assert_eq!(layout_width(Some(80), Some(OsStr::new("100"))), 80);
        assert_eq!(layout_width(Some(200), None), 120);
        assert_eq!(layout_width(Some(119), None), 119);
        assert_eq!(layout_width(None, Some(OsStr::new("100"))), 100);
        assert_eq!(layout_width(None, Some(OsStr::new(" 90 "))), 90);
        assert_eq!(layout_width(None, Some(OsStr::new("121"))), 120);
        assert_eq!(layout_width(Some(0), Some(OsStr::new("90"))), 90);
        for bad in ["", "0", "wide", "-5"] {
            assert_eq!(layout_width(None, Some(OsStr::new(bad))), 120, "{bad:?}");
        }
        assert_eq!(layout_width(None, None), MAX_WIDTH);
        assert_eq!(layout_width(Some(1), None), 1);
    }

    /// fang wraps the about at the width less its two-space indent:
    /// update-venv's about (sb-go's text) at 120 columns, its first line
    /// exactly 118; at 60 every line fits.
    #[test]
    fn the_about_wraps_under_its_indent() {
        let about = "Update the Ansible virtual environment from the current Saltbox checkout and refresh saltbox.fact without updating Git repositories";
        let cmd = Command::new("update-venv")
            .about("Update")
            .long_about(about);
        let at = |width| {
            render(
                &cmd,
                "sb update-venv",
                &[],
                &look(&Theme::plain(), false, width),
                false,
            )
        };
        assert!(
            at(120).starts_with(&format!(
                "  {}\n  repositories\n\n  USAGE\n",
                about.trim_end_matches(" repositories")
            )),
            "{}",
            at(120)
        );
        let narrow = at(60);
        assert!(narrow.lines().all(|l| l.width() <= 60), "{narrow}");
        assert!(
            narrow.starts_with(
                "  Update the Ansible virtual environment from the current\n  Saltbox checkout"
            ),
            "{narrow}"
        );
    }

    /// The help `args` shows once `apply` has run at `width`.
    fn shown_at(cmd: Command, args: &[&str], width: usize) -> String {
        apply(cmd, &Theme::plain(), false, width)
            .try_get_matches_from(args)
            .expect_err("help is shown as an error")
            .to_string()
    }

    /// Help leads with one blank line, as fang's, with an about or
    /// without one (then the USAGE heading comes next).
    #[test]
    fn help_leads_with_one_blank_line() {
        let bare =
            Command::new("x").arg(Arg::new("all").long("all").action(clap::ArgAction::SetTrue));
        assert!(
            shown_at(bare, &["x", "--help"], 120).starts_with("\n  USAGE\n\n"),
            "{}",
            shown_at(Command::new("x"), &["x", "--help"], 120)
        );
        assert!(shown_at(sb(), &["sb", "--help"], 120).starts_with("\n  Saltbox CLI\n\n  USAGE\n"));
    }

    /// A row's text wraps at the width under the help column: a default
    /// with its help, a long help's lines each, its blank line left out.
    #[test]
    fn rows_wrap_under_the_help_column() {
        assert_eq!(
            shown_at(with_long_help(), &["sb", "install", "--help"], 50),
            "\n  Runs Ansible playbooks.\n\n  Tags select what runs.\n\n  USAGE\n\n    sb install [tags] [--flags]\n\n  FLAGS\n\n    --color          Controls when to use color\n                     (auto)\n    -e --extra-vars  Extra variables\n                     Each value reaches Ansible\n                     whole.\n    -h --help        Help for install (--help\n                     shows more details)\n    -s --skip-tags   Tags to skip\n"
        );
        let root = shown_at(sb(), &["sb", "--help"], 60);
        assert!(
            root.contains("    help [command]            Print this message or the help\n                              of the given subcommand(s)\n"),
            "{root}"
        );
        assert!(root.lines().all(|l| l.width() <= 60), "{root}");
        // Wide enough, nothing changes.
        assert_eq!(
            shown_at(with_long_help(), &["sb", "install", "--help"], 120),
            shown(with_long_help(), &["sb", "install", "--help"])
        );
    }

    /// With colour, each wrapped piece is styled on its own: no row's
    /// colour runs into the next row's indent.
    #[test]
    fn each_wrapped_piece_takes_the_text_style() {
        let theme = Theme::resolve(&Palette::default(), Profile::TrueColor);
        let text = theme.style(Role::Text);
        let out = render(
            &with_long_help()
                .find_subcommand("install")
                .expect("install")
                .clone(),
            "sb install",
            &[],
            &look(&theme, true, 50),
            true,
        );
        assert!(
            out.contains(&format!("{text}Each value reaches Ansible{text:#}\n                     {text}whole.{text:#}\n")),
            "{out:?}"
        );
    }

    /// A word wider than the row breaks at any character, from where it
    /// starts (tabled's rule); a terminal too
    /// narrow for the help column wraps the text at 20 columns, past the
    /// edge, rather than a character a row.
    #[test]
    fn narrow_rows_keep_twenty_columns() {
        assert_eq!(
            wrapped("see https://docs.example/a/very/long/path/to/a/page", 20),
            [
                "see https://docs.exa",
                "mple/a/very/long/pat",
                "h/to/a/page"
            ]
        );
        let out = shown_at(with_long_help(), &["sb", "install", "--help"], 10);
        assert!(
            out.contains(
                "    --color          Controls when to use\n                     color (auto)\n"
            ),
            "{out}"
        );
    }

    /// A wrapped line keeps its own indent on its first piece and its own
    /// trailing space on its last; the spaces at each break go; a line of
    /// spaces only is one empty piece.
    #[test]
    fn a_wrapped_line_keeps_its_own_ends() {
        assert_eq!(wrapped("  aaaa bbbb cccc ", 8), ["  aaaa", "bbbb", "cccc "]);
        assert_eq!(wrapped(&" ".repeat(30), 8), [""]);
    }

    /// A row's text writes no blank line: a paragraph break, a line of
    /// spaces and a trailing new line are left out; a flag with no help
    /// still has its row.
    #[test]
    fn rows_have_no_blank_lines() {
        let mut out = String::new();
        write_row(
            &mut out,
            "    -x  ",
            "one\n\n   \ntwo\n",
            8,
            120,
            Style::new(),
        );
        assert_eq!(out, "    -x  one\n        two\n");
        let mut out = String::new();
        write_row(&mut out, "    -y  ", "", 8, 120, Style::new());
        assert_eq!(out, "    -y  \n");
    }
}
