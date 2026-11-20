//! `--color` on every output path (spec: startup step 5; ADR-002).
//!
//! The flag is colorchoice-clap's (the anstream project's clap mixin), and
//! its choice becomes anstream's global choice, which every anstream output
//! reads first, clap's own help and errors included (`clap_builder` 4.6.7
//! `output/fmt.rs` prints through `anstream::AutoStream`; anstream 1.0.0
//! `auto.rs` reads `ColorChoice::global()` for `Auto`). clap prints help
//! while it parses, so the choice has to be known before the real parse.
//!
//! [`early_choice`] runs the same command once as clap's `ignore_errors`
//! partial parse (its doc example reads `--config` despite later errors:
//! <https://docs.rs/clap/4.6.7/clap/struct.Command.html#method.ignore_errors>),
//! with the help flag as a plain flag and `help` as a plain subcommand, so
//! nothing is printed and parsing carries on past them. A parse that
//! succeeds is final. Only when it fails (the run will end in clap's exit-2
//! error) does the partial parse (`ignore_errors`, which stops at the first
//! error) and then clap's own lexer (`clap_lex`, `--color=V` or `--color V`
//! before `--`) look for the flag, so even that error message follows it;
//! the lexer may then take another option's value for the flag, which only
//! colours that error. `main` re-applies the real parse's choice.
//! [`early_read`] also says, from the same parse, whether the line asks
//! for help, so sb lays its help out only then. When console or
//! crossterm output lands (term-ui), `always`/`never` are passed on to
//! them too.
//!
//! Why not a clap API: there is none. Built-in help "is acted on
//! immediately" (open issue <https://github.com/clap-rs/clap/issues/6013>,
//! waiting on design); its suggested workaround, a help flag of our own
//! with help rendered by hand, means re-implementing help and its error
//! flow for every subcommand.

use std::ffi::OsString;

use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ColorChoice, Command, FromArgMatches};
pub use colorchoice_clap::Color;

/// The `--color` choice in `args`, read before the real parse. Missing,
/// bad or unreachable values give `auto`; the real parse reports bad ones.
pub fn early_choice<I, T>(cmd: Command, args: I) -> Color
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    early_read(cmd, args).color
}

/// What the pre-pass reads from the command line before the real parse.
#[derive(Debug)]
pub struct Early {
    /// The `--color` choice, as [`early_choice`] gives it.
    pub color: Color,
    /// Whether clap will answer the line with help: `-h` or `--help`
    /// anywhere before `--` (in a cluster too), the `help` subcommand, or
    /// a command clap shows help for when it's alone
    /// (`arg_required_else_help`, bare `sb` included). A line the
    /// pre-pass can't parse reads `false`, unless clap stopped it to show
    /// help or saw a help flag before its error; the real parse may still
    /// show help for it (`-h` after a typo it can't get past).
    pub help: bool,
    /// The pre-pass's matches when its parse succeeded, so is final. On
    /// a line that asks for no help ([`Early::help`]) they are the real
    /// parse's, but for the help flag the pre-pass adds (`help`, false),
    /// given a tree whose only `help` subcommands are clap's (none of
    /// whose commands takes a positional): the trees differ only in the
    /// help flag, the `help` subcommands and the version flag, and such a
    /// line uses none of them. The caller can read its command from them
    /// rather than build the tree and parse the line again.
    pub matches: Option<clap::ArgMatches>,
    /// On a line that asks for help, the commands it names, outermost
    /// first (by their names, not the aliases given), when the pre-pass
    /// can tell whose help clap shows: its parse succeeded, or stopped at
    /// a command shown alone, or saw a help flag before its error. `sb
    /// help <commands>` names the commands after `help`. None: lay out
    /// every command's help (a group's own `help` subcommand, `-h` after
    /// a typo).
    pub path: Option<Vec<String>>,
}

/// The subcommands `matches` holds, outermost first; after the
/// pre-pass's plain `help` subcommand, the words given to it, as clap's
/// own `help` reads them.
fn named(matches: &clap::ArgMatches) -> Vec<String> {
    let mut path = Vec::new();
    let mut at = matches;
    while let Some((name, sub)) = at.subcommand() {
        if name == "help" && path.is_empty() {
            return sub
                .try_get_many::<String>("command")
                .ok()
                .flatten()
                .map_or_default(|names| names.cloned().collect());
        }
        path.push(name.to_owned());
        at = sub;
    }
    path
}

/// [`early_choice`]'s pre-pass, also saying whether the line asks for
/// help ([`Early::help`]): read from the same parse, so it costs nothing
/// more, and in clap's own terms (clusters, `--`, option values).
pub fn early_read<I, T>(cmd: Command, args: I) -> Early
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    // `-h` is claimed for every subcommand here; a subcommand that defines
    // its own `-h` would conflict (clap's debug assertions say so).
    let pre_pass = cmd
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .disable_version_flag(true)
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::SetTrue)
                .global(true),
        )
        // `help` as a plain subcommand, so the pre-pass reads a `--color`
        // after it. clap's real `help` takes no flags, so `sb help
        // --color=never` is a usage error; this makes that error follow it.
        .subcommand(Command::new("help").arg(Arg::new("command").num_args(0..)));
    let from_command_line = |matches: &clap::ArgMatches| {
        (matches.value_source("color") == Some(ValueSource::CommandLine))
            .then(|| Color::from_arg_matches(matches).ok())
            .flatten()
    };
    // A parse that succeeds is final: its `--color`, or `auto`. Only when
    // it fails (the run ends in clap's exit-2 error) do the partial parse
    // and then the lexer look further, and then only that error's colour
    // depends on them.
    // The help flag is global, so the root's matches hold it wherever it
    // was given. Read without `get_flag`, which panics on a partial
    // parse: `ignore_errors` sets no defaults, so an unset flag has no
    // value there.
    let asks_help = |matches: &clap::ArgMatches| {
        matches!(matches.try_get_one::<bool>("help"), Ok(Some(true)))
            || matches.subcommand_name() == Some("help")
    };
    match pre_pass.clone().try_get_matches_from(&args) {
        Ok(matches) => {
            let help = asks_help(&matches);
            Early {
                help,
                color: from_command_line(&matches).unwrap_or_default(),
                path: help.then(|| named(&matches)),
                matches: Some(matches),
            }
        }
        Err(err) => {
            let partial = pre_pass
                .ignore_errors(true)
                .try_get_matches_from(&args)
                .ok();
            let alone =
                err.kind() == clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand;
            let flagged = partial.as_ref().is_some_and(asks_help);
            Early {
                // A command shown alone still stops the pre-pass with
                // help (and clap's own help would, should it answer); a
                // help flag before the error (`sb --help`, which lacks a
                // command) is in the partial parse. A group's own `help`
                // (`sb docker help`) reads as none here: the real parse
                // answers it, with every command laid out.
                help: alone || err.kind() == clap::error::ErrorKind::DisplayHelp || flagged,
                // Whose help, where the partial parse knows it: the
                // command shown alone, or the one the help flag was given
                // to, clap acting on the flag where it meets it.
                path: partial.as_ref().filter(|_| alone || flagged).map(named),
                color: partial
                    .as_ref()
                    .and_then(from_command_line)
                    .unwrap_or_else(|| Color {
                        color: lexed_choice(args).unwrap_or(ColorChoice::Auto),
                    }),
                matches: None,
            }
        }
    }
}

/// `--color=V` or `--color V` before `--`, found by clap's lexer (its crate
/// example's loop: <https://docs.rs/clap_lex/1.1.1/clap_lex/>), for when a
/// parse error stopped the pre-pass first. The last valid value wins, as in
/// the real parse.
fn lexed_choice(args: Vec<OsString>) -> Option<ColorChoice> {
    let raw = clap_lex::RawArgs::new(args);
    let mut cursor = raw.cursor();
    raw.next(&mut cursor); // the program name
    let mut found = None;
    while let Some(arg) = raw.next(&mut cursor) {
        if arg.is_escape() {
            break;
        }
        if let Some((Ok("color"), value)) = arg.to_long() {
            // A flag with no value or a non-UTF-8 one is a usage error for
            // the real parse; keep whatever was found before it.
            let Some(value) = value.or_else(|| raw.next_os(&mut cursor)) else {
                break;
            };
            // clap's own `FromStr`, case-sensitive like the real parse; a
            // bad value keeps an earlier good one.
            if let Some(choice) = value.to_str().and_then(|v| v.parse::<ColorChoice>().ok()) {
                found = Some(choice);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{ColorChoice, CommandFactory, Parser, Subcommand};

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        color: Color,
        #[command(subcommand)]
        command: Sub,
    }

    #[derive(Subcommand)]
    enum Sub {
        Version {
            #[arg(long)]
            json: bool,
        },
        /// Passes its arguments on, like a future ansible pass-through.
        Run {
            #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
            rest: Vec<String>,
        },
    }

    /// The pre-pass's help, in clap's terms: no arguments (clap shows help
    /// for a missing command), the flag anywhere before `--`, with no
    /// command too, and the `help` subcommand; not after `--` or past a
    /// typo.
    #[test]
    fn the_pre_pass_says_when_help_is_asked() {
        let help = |args: &[&str]| {
            let all = std::iter::once("sb").chain(args.iter().copied());
            early_read(Cli::command(), all).help
        };
        for args in [
            &[][..],
            &["-h"],
            &["--help"],
            &["version", "--help"],
            &["version", "-h", "--json"],
            &["help"],
            &["help", "version"],
        ] {
            assert!(help(args), "{args:?}");
        }
        for args in [
            &["version"][..],
            &["run", "x", "-h"],
            &["run", "--", "--help"],
            &["bogus", "-h"],
            // A partial parse sets no defaults, the help flag's included.
            &["--color", "bogus"],
        ] {
            assert!(!help(args), "{args:?}");
        }
    }

    /// A parse that succeeds keeps its matches, help or not; one that
    /// fails has none.
    #[test]
    fn a_final_parse_keeps_its_matches() {
        let read = |args: &[&str]| {
            let all = std::iter::once("sb").chain(args.iter().copied());
            early_read(Cli::command(), all)
        };
        let early = read(&["--color=never", "version", "--json"]);
        let matches = early.matches.expect("final");
        let (name, version) = matches.subcommand().expect("a command");
        assert_eq!(name, "version");
        assert!(version.get_flag("json"));
        assert!(read(&["version", "--help"]).matches.is_some());
        for args in [&["bogus"][..], &["version", "--bogus"], &[]] {
            assert!(read(args).matches.is_none(), "{args:?}");
        }
    }

    /// Whose help a help line shows: the commands named, after `help`
    /// too; the root's for `-h` before a mistake; none known after one,
    /// nor on a line that asks for no help.
    #[test]
    fn the_pre_pass_names_whose_help_is_shown() {
        let path = |args: &[&str]| {
            let all = std::iter::once("sb").chain(args.iter().copied());
            early_read(Cli::command(), all).path
        };
        let names =
            |names: &[&str]| Some(names.iter().map(ToString::to_string).collect::<Vec<_>>());
        assert_eq!(path(&["version", "--help"]), names(&["version"]));
        assert_eq!(path(&["help", "version"]), names(&["version"]));
        assert_eq!(path(&["help"]), names(&[]));
        assert_eq!(path(&["-h"]), names(&[]));
        assert_eq!(path(&[]), names(&[]));
        assert_eq!(path(&["-h", "bogus"]), names(&[]));
        assert_eq!(path(&["version", "-h", "--bogus"]), names(&["version"]));
        assert_eq!(path(&["bogus", "-h"]), None);
        assert_eq!(path(&["version"]), None);
        assert_eq!(path(&["version", "--bogus"]), None);
    }

    fn choice(args: &[&str]) -> ColorChoice {
        let mut all = vec!["sb"];
        all.extend_from_slice(args);
        early_choice(Cli::command(), all).color
    }

    #[test]
    fn the_flag_is_read_wherever_it_is() {
        assert_eq!(choice(&["--color=never", "version"]), ColorChoice::Never);
        assert_eq!(
            choice(&["version", "--color", "always"]),
            ColorChoice::Always
        );
        assert_eq!(
            choice(&["version", "--json", "--color=never"]),
            ColorChoice::Never
        );
    }

    /// `--help` doesn't stop the pre-pass, before or after the flag.
    #[test]
    fn help_does_not_hide_the_flag() {
        assert_eq!(choice(&["--help", "--color", "never"]), ColorChoice::Never);
        assert_eq!(choice(&["-h", "--color=always"]), ColorChoice::Always);
        assert_eq!(
            choice(&["version", "--help", "--color=never"]),
            ColorChoice::Never
        );
    }

    /// After `help` (a usage error in the real parse) the flag is still read,
    /// so the error follows it.
    #[test]
    fn the_help_subcommand_does_not_hide_the_flag() {
        assert_eq!(choice(&["help", "--color=never"]), ColorChoice::Never);
        assert_eq!(
            choice(&["help", "version", "--color", "always"]),
            ColorChoice::Always
        );
    }

    /// An error before the flag (an unknown option or subcommand) stops the
    /// pre-pass; clap's lexer still finds the flag, so the error message
    /// follows it too.
    #[test]
    fn a_parse_error_before_the_flag_does_not_hide_it() {
        assert_eq!(
            choice(&["version", "--bogus", "--color=never"]),
            ColorChoice::Never
        );
        assert_eq!(choice(&["bogus", "--color", "always"]), ColorChoice::Always);
    }

    /// After `--` nothing is an option, so a `--color` there isn't ours.
    #[test]
    fn a_flag_after_the_escape_is_not_read() {
        assert_eq!(choice(&["bogus", "--", "--color=never"]), ColorChoice::Auto);
    }

    /// The lexer matches values as the real parse does, case-sensitively:
    /// a value the real parse rejects gives `auto`.
    #[test]
    fn the_lexer_rejects_what_the_real_parse_rejects() {
        assert_eq!(choice(&["bogus", "--color", "NEVER"]), ColorChoice::Auto);
    }

    /// A later malformed `--color` (no value) doesn't throw away an earlier
    /// one the lexer found.
    #[test]
    fn a_trailing_flag_without_a_value_keeps_the_earlier_one() {
        assert_eq!(
            choice(&["bogus", "--color=never", "--color"]),
            ColorChoice::Never
        );
    }

    /// A parse that succeeds is final: a `--color` that belongs to a
    /// pass-through's arguments is theirs, not sb's (the lexer never runs).
    #[test]
    fn a_successful_parse_is_never_second_guessed() {
        assert_eq!(choice(&["run", "foo", "--color=never"]), ColorChoice::Auto);
        assert_eq!(
            choice(&["--color=always", "run", "foo", "--color=never"]),
            ColorChoice::Always
        );
    }

    /// A bad later value doesn't overwrite a good earlier one the lexer
    /// found (only the exit-2 error's colour depends on it).
    #[test]
    fn a_bad_later_value_keeps_the_earlier_one() {
        assert_eq!(
            choice(&["bogus", "--color=never", "--color=bogus"]),
            ColorChoice::Never
        );
    }

    #[test]
    fn no_or_bad_flag_means_auto() {
        assert_eq!(choice(&["version"]), ColorChoice::Auto);
        assert_eq!(choice(&["--color", "bogus", "version"]), ColorChoice::Auto);
        assert_eq!(choice(&["--color"]), ColorChoice::Auto);
    }
}
