//! What sb's full-screen commands share around sb-tui's screens (`sb
//! logs`, `sb edit`'s picker, `sb fact`): the theme they draw with and the
//! run's result once a screen has closed.

use sb_cli_core::exit::Exit;
use sb_cli_core::theme;
use sb_tui::Ending;

/// The theme sb's screens draw with: the run's, or none when stdout
/// gets no colour (`NO_COLOR`, `--color=never`; ADR-002 decision 4, the
/// tui spec's "Text path"), read as sb's other output reads it
/// (`colour_on`, anstream's choice). crossterm drops only colours under
/// `NO_COLOR` and doesn't know `--color`, so without this the viewer
/// lit a match with a background that isn't drawn; the plain theme's
/// `ascii` lights it in reverse video (tui decision 23). Left as it is:
/// `NO_COLOR` with `--color=always` gets the run's theme, whose colours
/// crossterm still drops (crossterm's `force_color_output` would follow
/// the choice; no Saltbox use sets both).
pub fn theme() -> theme::Theme {
    if sb_term_ui::colour_on(sb_term_ui::Stream::Stdout) {
        theme::current().clone()
    } else {
        theme::Theme::plain()
    }
}

/// The run's result after the screens: 130 after Ctrl-C, quietly (tui
/// decision 13).
///
/// # Errors
///
/// `Exit(130)` after Ctrl-C.
pub fn ended(ending: Ending) -> anyhow::Result<()> {
    match ending {
        Ending::Interrupted => Err(Exit(130).into()),
        // A signal's ending is finish_run's to show.
        Ending::Quit | Ending::Cancelled => Ok(()),
    }
}
