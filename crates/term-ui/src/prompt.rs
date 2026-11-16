//! Prompts (term-ui spec, "Prompts"): dialoguer's, on the terminal, never
//! stdin, and never while the runner draws. Built with their first user:
//! the confirm with `release`'s self-update (term-ui decision 1).
//!
//! The terminal is `/dev/tty`, opened here: under `curl … | sudo bash`
//! stdin is the script. console reads keys from stdin when stdin is the
//! terminal, else from `/dev/tty` itself (console 0.16.6
//! `src/unix_term.rs`, `Input::unbuffered`), and dialoguer draws on the
//! `Term` it is given, here a read-write pair on the opened terminal
//! (<https://docs.rs/console/0.16.6/console/struct.Term.html#method.read_write_pair>).
//! With no terminal, or from a background process group (a read would
//! stop sb with SIGTTIN, a hang), the caller's flag is the way to answer,
//! and the error says so.

use std::fmt;
use std::fs::{File, OpenOptions};

use dialoguer::Input;
use dialoguer::theme::Theme;
use sb_cli_core::cancel::Shutdown;
use sb_cli_core::theme::Role;

use crate::style::paint;

/// Why a prompt got no answer.
#[derive(Debug, thiserror::Error)]
pub enum PromptError {
    /// No terminal to ask on, or sb runs in the background: the caller's
    /// flag answers instead.
    #[error("can't ask \"{question}\": {reason}; answer it with {flag}")]
    NoTerminal {
        question: String,
        reason: String,
        flag: &'static str,
    },
    /// A runner is drawing: a prompt would draw over its live lines (a
    /// bug in the command).
    #[error("can't ask \"{0}\" while the runner draws")]
    RunnerLive(String),
    /// Ctrl-C while asking: console raises SIGINT for it, and the confirm
    /// has the run's [`Shutdown`] record that delivery before it returns
    /// ([`Shutdown::interrupt_raised`]), so cli-core's `finish_run` ends
    /// sb by the signal, however late the signals thread reads it.
    #[error("interrupted")]
    Interrupted,
    /// Reading or drawing failed.
    #[error("can't ask \"{question}\": {error}")]
    Failed { question: String, error: String },
}

/// Ask `question`, yes or no, default no: `answer` when the caller has one
/// (its `flag`, such as `--yes`), else on the terminal; `shutdown` is the
/// run's, which a Ctrl-C at the question interrupts. The whole line
/// typed before Enter is the answer: `y` or `yes`, in any case, with
/// spaces around trimmed, accepts; anything else (`nay`, `no thank you`,
/// Enter alone) declines. It guards replacing a root-run binary, so a
/// stray `y` inside another word never accepts.
///
/// # Errors
///
/// [`PromptError::NoTerminal`] naming `flag` when there is no terminal or
/// sb is in the background; [`PromptError::RunnerLive`] while a runner is
/// live; [`PromptError::Interrupted`] on Ctrl-C, the run then interrupted
/// by SIGINT in `shutdown`;
/// [`PromptError::Failed`] when the terminal fails.
pub fn confirm(
    question: &str,
    answer: Option<bool>,
    flag: &'static str,
    shutdown: &Shutdown,
) -> Result<bool, PromptError> {
    confirm_on(
        question,
        answer,
        flag,
        shutdown,
        crate::runner_live(),
        || OpenOptions::new().read(true).write(true).open("/dev/tty"),
    )
}

/// Whether a question can be asked here, by [`confirm`]'s rule: `/dev/tty`
/// opens and sb is in its foreground process group. For a command that
/// must know before its first step whether its questions can be asked
/// (`sb update`'s up-front flag check, ops spec).
///
/// # Errors
///
/// Why not, in the confirm's words: `no terminal (<cause>)` or `sb runs
/// in the background`.
pub fn can_ask() -> Result<(), String> {
    open_terminal(|| OpenOptions::new().read(true).write(true).open("/dev/tty")).map(drop)
}

/// The terminal from `terminal`, if sb is in its foreground process group
/// (a read from the background stops sb; rustix's `tcgetpgrp`,
/// <https://docs.rs/rustix/1.1.6/rustix/termios/fn.tcgetpgrp.html>).
fn open_terminal(terminal: impl FnOnce() -> std::io::Result<File>) -> Result<File, String> {
    let tty = terminal().map_err(|err| format!("no terminal ({err})"))?;
    let foreground =
        rustix::termios::tcgetpgrp(&tty).is_ok_and(|group| group == rustix::process::getpgrp());
    if !foreground {
        return Err("sb runs in the background".to_owned());
    }
    Ok(tty)
}

/// [`confirm`] with the terminal from `terminal`, `runner_live` saying
/// whether a runner is live (a parameter, so a test doesn't read the
/// process-wide count that other tests' runners raise at the same time).
fn confirm_on(
    question: &str,
    answer: Option<bool>,
    flag: &'static str,
    shutdown: &Shutdown,
    runner_live: bool,
    terminal: impl FnOnce() -> std::io::Result<File>,
) -> Result<bool, PromptError> {
    if let Some(answer) = answer {
        return Ok(answer);
    }
    if runner_live {
        return Err(PromptError::RunnerLive(question.to_owned()));
    }
    let no_terminal = |reason: String| PromptError::NoTerminal {
        question: question.to_owned(),
        reason,
        flag,
    };
    let tty = open_terminal(terminal).map_err(no_terminal)?;
    let failed = |error: String| PromptError::Failed {
        question: question.to_owned(),
        error,
    };
    // What was typed before the question showed is dropped, so a stray
    // `y` and Enter (or a paste) can't answer a question never seen
    // (rustix's `tcflush`,
    // <https://docs.rs/rustix/1.1.6/rustix/termios/fn.tcflush.html>). The
    // terminal's input queue is one, whichever descriptor reads it.
    let _ = rustix::termios::tcflush(&tty, rustix::termios::QueueSelector::IFlush);
    let write = tty.try_clone().map_err(|err| failed(err.to_string()))?;
    let term = console::Term::read_write_pair(tty, write);
    let theme = YesNo {
        palette: Palette {
            colour: console::colors_enabled_stderr(),
        },
    };
    // dialoguer's text input reads the whole line, empty allowed
    // (<https://docs.rs/dialoguer/0.12.0/dialoguer/struct.Input.html#method.interact_text_on>,
    // `allow_empty`). Why not its `Confirm`: with `wait_for_newline` it
    // keeps the last `y` or `n` typed and ignores other keys, so `nay`
    // accepts (0.12.0 `src/prompts/confirm.rs`). Why no validator: one
    // would ask again, and anything but `y`/`yes` is a plain no.
    Input::<String>::with_theme(&theme)
        .with_prompt(question)
        .allow_empty(true)
        .interact_text_on(&term)
        .map(|line| accepts(&line))
        .map_err(|dialoguer::Error::IO(err)| {
            // dialoguer's `Input` never hides the cursor (its `Confirm`
            // did); still, after Ctrl-C or a failed read, the cursor is
            // shown whatever drew last (console's own call,
            // <https://docs.rs/console/0.16.6/console/struct.Term.html#method.show_cursor>).
            // UNVERIFIED: dialoguer documents no pattern for it; checked
            // against its source (0.12.0 `src/prompts/input.rs` returns the
            // read's error as it is).
            let _ = term.show_cursor();
            // And the question's line ended, so what follows starts clean.
            let _ = term.write_line("");
            if err.kind() == std::io::ErrorKind::Interrupted {
                // console raised SIGINT before returning (UNVERIFIED, from
                // its source: `read_single_key`,
                // <https://docs.rs/console/0.16.6/src/console/unix_term.rs.html>):
                // recorded now, not when the signals thread gets to it, or
                // the run could end with exit 1 before the thread records
                // it. Why not console's `Term::read_key_raw`, which returns
                // `Key::CtrlC` without raising: dialoguer 0.12.0's `Input`
                // reads with `read_key` (`src/prompts/input.rs`), and
                // replacing `Input` would mean line editing of our own.
                let _ = shutdown.interrupt_raised();
                PromptError::Interrupted
            } else {
                failed(err.to_string())
            }
        })
}

/// The confirm's answer from the whole line: `y` or `yes`, in any case,
/// spaces around trimmed (release spec, "Differences from sb-go" 8;
/// term-ui spec, "Prompts").
fn accepts(line: &str) -> bool {
    let line = line.trim();
    line.eq_ignore_ascii_case("y") || line.eq_ignore_ascii_case("yes")
}

/// The prompts' colours: term-ui's anstyle painting from the palette. Why
/// not dialoguer's `ColorfulTheme`: its fields are console's styles, which
/// write a bright colour as a 256-colour code (see `style.rs`), wrong
/// under the `ansi` profile; dialoguer's `Theme` trait is its documented
/// way to restyle
/// (<https://docs.rs/dialoguer/0.12.0/dialoguer/theme/trait.Theme.html>).
struct Palette {
    colour: bool,
}

impl Palette {
    fn paint(&self, role: Role, text: &str) -> String {
        paint(sb_cli_core::theme::current(), role, self.colour, text)
    }
}

/// The confirm's look on dialoguer's text input: the `Theme` trait's
/// confirm layout (`[y/N]`) with `ColorfulTheme`'s `?` prefix, and the
/// answer shown as `yes` or `no` from the line, as the confirm shows it.
struct YesNo {
    palette: Palette,
}

impl Theme for YesNo {
    fn format_input_prompt(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        _default: Option<&str>,
    ) -> fmt::Result {
        write!(
            f,
            "{} {prompt} {} ",
            self.palette.paint(Role::Accent, "?"),
            self.palette.paint(Role::Muted, "[y/N]")
        )
    }

    fn format_input_prompt_selection(
        &self,
        f: &mut dyn fmt::Write,
        prompt: &str,
        line: &str,
    ) -> fmt::Result {
        let answer = if accepts(line) {
            self.palette.paint(Role::Ok, "yes")
        } else {
            self.palette.paint(Role::Muted, "no")
        };
        write!(
            f,
            "{} {prompt} {answer}",
            self.palette.paint(Role::Accent, "?")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_tty() -> std::io::Result<File> {
        Err(std::io::Error::from_raw_os_error(6))
    }

    #[test]
    fn a_given_answer_is_used_without_asking() {
        for answer in [true, false] {
            let asked = confirm_on(
                "Do you want to update",
                Some(answer),
                "--yes",
                &Shutdown::new(),
                true,
                || unreachable!("not asked"),
            );
            assert_eq!(asked.expect("answered"), answer);
        }
    }

    /// No terminal and no answer: an error naming the flag, never a hang.
    #[test]
    fn without_a_terminal_the_error_names_the_flag() {
        let err = confirm_on(
            "Do you want to update",
            None,
            "--yes",
            &Shutdown::new(),
            false,
            no_tty,
        )
        .expect_err("no terminal");
        assert!(matches!(err, PromptError::NoTerminal { flag: "--yes", .. }));
        assert_eq!(
            err.to_string(),
            "can't ask \"Do you want to update\": no terminal (No such device or address \
             (os error 6)); answer it with --yes"
        );
    }

    /// The rule on its own, for a command's up-front check: no terminal,
    /// or one sb isn't in the foreground of, is a reason; the same
    /// reasons as the confirm's.
    #[test]
    fn the_terminal_rule_gives_the_confirms_reasons() {
        assert_eq!(
            open_terminal(no_tty).expect_err("no terminal"),
            "no terminal (No such device or address (os error 6))"
        );
        let file = tempfile::tempfile().expect("file");
        assert_eq!(
            open_terminal(|| Ok(file)).expect_err("not a terminal"),
            "sb runs in the background"
        );
    }

    /// While a runner draws, nothing is asked (the question would share
    /// the screen with its live lines), terminal or not.
    #[test]
    fn while_a_runner_draws_nothing_is_asked() {
        let err = confirm_on(
            "Do you want to update",
            None,
            "--yes",
            &Shutdown::new(),
            true,
            || unreachable!("no terminal opened"),
        )
        .expect_err("refused");
        assert_eq!(
            err.to_string(),
            "can't ask \"Do you want to update\" while the runner draws"
        );
    }

    /// A file that isn't a terminal reads as the background (its group
    /// can't be read), so it is refused the same way.
    #[test]
    fn a_file_that_isnt_a_terminal_is_refused() {
        let file = tempfile::tempfile().expect("file");
        let err = confirm_on(
            "Do you want to update",
            None,
            "--yes",
            &Shutdown::new(),
            false,
            || Ok(file),
        )
        .expect_err("not a terminal");
        assert_eq!(
            err.to_string(),
            "can't ask \"Do you want to update\": sb runs in the background; answer it \
             with --yes"
        );
    }

    /// The whole line is the answer: `y` or `yes` in any case, spaces
    /// around trimmed; anything else declines, never the last `y` typed.
    #[test]
    fn only_y_or_yes_accepts() {
        for line in ["y", "Y", " yes ", "YES", "Yes", "\tyes"] {
            assert!(accepts(line), "{line:?}");
        }
        for line in [
            "",
            "  ",
            "n",
            "no",
            "nay",
            "no thank you",
            "yes please",
            "ye",
            "yy",
            "y y",
        ] {
            assert!(!accepts(line), "{line:?}");
        }
    }

    fn rendered(colour: bool, render: impl Fn(&YesNo, &mut String) -> fmt::Result) -> String {
        let mut text = String::new();
        render(
            &YesNo {
                palette: Palette { colour },
            },
            &mut text,
        )
        .expect("render");
        text
    }

    #[test]
    fn the_prompt_is_dialoguers_layout() {
        let asked = rendered(false, |theme, f| {
            theme.format_input_prompt(f, "Do you want to update", None)
        });
        assert_eq!(asked, "? Do you want to update [y/N] ");
        let answered = |line| {
            rendered(false, |theme, f| {
                theme.format_input_prompt_selection(f, "Do you want to update", line)
            })
        };
        assert_eq!(answered(" Yes "), "? Do you want to update yes");
        assert_eq!(answered("nay"), "? Do you want to update no");
        assert_eq!(answered(""), "? Do you want to update no");
    }

    /// With colour, the marks are painted; the question stays plain.
    #[test]
    fn colour_paints_the_marks_only() {
        let text = rendered(true, |theme, f| {
            theme.format_input_prompt(f, "Do you want to update", None)
        });
        let theme = sb_cli_core::theme::current();
        assert_eq!(
            text,
            format!(
                "{} Do you want to update {} ",
                paint(theme, Role::Accent, true, "?"),
                paint(theme, Role::Muted, true, "[y/N]")
            )
        );
    }
}
