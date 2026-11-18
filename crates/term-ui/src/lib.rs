//! sb's inline terminal output (term-ui spec): colour on every inline
//! path, theme adapters, and later the task runner and tables.

mod child;
mod prompt;
mod runner;
pub mod style;
mod table;
mod text;
mod tree;
mod tty;

pub use child::ChildFailure;
pub use prompt::{PromptError, can_ask, confirm};
pub use runner::{
    Group, GroupAll, Labels, Line, Options, Runner, Step, Target, keeps_codes, runner_live,
};
pub use table::{Cell, Table};
pub use text::{DisplayText, Resume, wrap_line};

use anstream::{AutoStream, ColorChoice};
use sb_cli_core::theme::Profile;

/// Where output goes; each stream follows its own colour choice (the
/// runner and prompts draw on stderr, tables and results on stdout).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// stdout's terminal width, none when stdout isn't a terminal (console's
/// `size_checked`:
/// <https://docs.rs/console/0.16.6/console/struct.Term.html#method.size_checked>):
/// what tables fit to, and `sb list`'s columns.
#[must_use]
pub fn stdout_width() -> Option<usize> {
    console::Term::stdout()
        .size_checked()
        .map(|(_, cols)| usize::from(cols))
}

/// Whether `stream` gets colour: anstream's resolved choice for it
/// (`--color`, `NO_COLOR`, `CLICOLOR_FORCE`, a terminal or not;
/// <https://docs.rs/anstream/1.0.0/anstream/struct.AutoStream.html#method.choice>).
#[must_use]
pub fn colour_on(stream: Stream) -> bool {
    let choice = match stream {
        Stream::Stdout => AutoStream::choice(&std::io::stdout()),
        Stream::Stderr => AutoStream::choice(&std::io::stderr()),
    };
    choice != ColorChoice::Never
}

/// Make console, and indicatif and dialoguer on top of it, follow sb's
/// colour choice: called by sb's `main` right after the real parse's
/// choice is written (`cli.color.write_global()`). Both switches are set
/// every time, so console's own `auto` rules can't disagree; no library
/// links anstream's choice to console (proto-inline's finding;
/// <https://docs.rs/console/0.16.6/console/fn.set_colors_enabled.html>,
/// <https://docs.rs/console/0.16.6/console/fn.set_colors_enabled_stderr.html>).
/// Under `ascii` both are off, so nothing styled through console writes
/// colour either.
pub fn follow_colour_choice(profile: Profile) {
    set_console_switches(
        AutoStream::choice(&std::io::stdout()),
        AutoStream::choice(&std::io::stderr()),
        profile,
    );
}

fn set_console_switches(stdout: ColorChoice, stderr: ColorChoice, profile: Profile) {
    let ascii = profile == Profile::Ascii;
    console::set_colors_enabled(!ascii && stdout != ColorChoice::Never);
    console::set_colors_enabled_stderr(!ascii && stderr != ColorChoice::Never);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn switches() -> (bool, bool) {
        (console::colors_enabled(), console::colors_enabled_stderr())
    }

    // anstream's choice and console's switches are process-wide, so one
    // test walks every case.
    #[test]
    fn console_and_colour_on_follow_the_choice_and_profile() {
        let cases = [
            (ColorChoice::Never, Profile::TrueColor, false),
            (ColorChoice::Always, Profile::TrueColor, true),
            (ColorChoice::AlwaysAnsi, Profile::Ansi, true),
            (ColorChoice::Always, Profile::Ascii, false),
        ];
        for (choice, profile, on) in cases {
            choice.write_global();
            follow_colour_choice(profile);
            assert_eq!(switches(), (on, on), "{choice:?} {profile:?}");
            let wanted = choice != ColorChoice::Never;
            assert_eq!(
                (colour_on(Stream::Stdout), colour_on(Stream::Stderr)),
                (wanted, wanted),
                "{choice:?}"
            );
        }
        ColorChoice::Auto.write_global();
        // Each stream on its own choice (injected: the global is shared).
        let mixed = [
            (
                ColorChoice::Always,
                ColorChoice::Never,
                Profile::TrueColor,
                (true, false),
            ),
            (
                ColorChoice::Never,
                ColorChoice::AlwaysAnsi,
                Profile::Ansi256,
                (false, true),
            ),
            (
                ColorChoice::Always,
                ColorChoice::Always,
                Profile::Ascii,
                (false, false),
            ),
        ];
        for (stdout, stderr, profile, wanted) in mixed {
            set_console_switches(stdout, stderr, profile);
            assert_eq!(switches(), wanted, "{stdout:?} {stderr:?} {profile:?}");
        }
    }
}
