//! Exit codes and the error block (spec: "Exit codes", "Errors").
//!
//! Commands return any `std::error::Error`; the binary uses anyhow, whose
//! errors expose their causes through `source()` like any other. Parse
//! errors never reach here: clap prints them and exits 2 itself.

use std::error::Error;
use std::fmt::Write as _;
use std::io::Write;
pub use std::process::ExitCode;

use crate::cancel::Shutdown;
use crate::theme::{self, Role, Theme};

/// End the run with this exit code and print nothing more: a wrapped
/// child's own code, or an error the command has already shown.
#[derive(Debug, thiserror::Error)]
#[error("exit code {0}")]
pub struct Exit(pub u8);

/// End a run: a signal shows "interrupted" (spec: a cancellation shows
/// that rather than an error chain) and exits with its code; a command's
/// own shutdown request exits quietly with its code; otherwise the result
/// decides, through [`report`] for an error.
///
/// Write to an anstream stream, so `--color` and NO_COLOR are honoured:
/// the block carries the theme's colour codes.
pub fn finish_run(
    shutdown: &Shutdown,
    error: Option<&(dyn Error + 'static)>,
    log_pointer: Option<&str>,
    out: &mut impl Write,
) -> ExitCode {
    if let Some(code) = shutdown.requested() {
        if shutdown.interrupted() {
            let block = block("interrupted", [], log_pointer, theme::current());
            let _ = out.write_all(block.as_bytes());
        }
        return ExitCode::from(code);
    }
    match error {
        None => ExitCode::SUCCESS,
        Some(err) => report(err, log_pointer, out),
    }
}

/// The error and its causes, outermost first. Why not `Error::sources()`:
/// still unstable (`error_iter`); why not anyhow's `Chain`: cli-core takes
/// any `std::error::Error` and doesn't depend on anyhow.
fn chain<'a>(err: &'a (dyn Error + 'static)) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    std::iter::successors(Some(err), |&e| e.source())
}

/// An error writing sb's own output (stdout or stderr). Commands wrap their
/// output errors in it, so a broken pipe here (and only here) means the
/// reader of sb's output went away.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct OutputError(std::io::Error);

/// Mark an error as one writing sb's own output.
pub fn output_error(err: std::io::Error) -> OutputError {
    OutputError(err)
}

/// The code an error ends the run with quietly, if it is a quiet exit: a
/// requested [`Exit`] anywhere in the chain, or a broken pipe on sb's own
/// output ([`OutputError`]). The reader went away (`sb ... | head`); Rust
/// ignores SIGPIPE, so this arrives as an error, and the run ends with the
/// code a process killed by SIGPIPE gets (128 + 13), as sb-go does. A broken
/// pipe anywhere else (a child sb writes to died) is a real failure, as in
/// sb-go, where only writes to fd 1 and 2 raise SIGPIPE. Why not `-Zon-broken-pipe=kill`:
/// unstable, so it would need an ADR
/// (https://doc.rust-lang.org/beta/unstable-book/compiler-flags/on-broken-pipe.html);
/// why not resetting SIGPIPE with libc: unsafe code, forbidden here. One
/// classification serves the screen ([`report`]) and the run log.
pub fn quiet_exit(err: &(dyn Error + 'static)) -> Option<u8> {
    if let Some(Exit(code)) = chain(err).find_map(|e| e.downcast_ref::<Exit>()) {
        return Some(*code);
    }
    chain(err)
        .filter_map(|e| e.downcast_ref::<OutputError>())
        .any(|e| e.0.kind() == std::io::ErrorKind::BrokenPipe)
        .then_some(141)
}

/// Print one error block (`✗` and the message, each cause on its own line,
/// then the log pointer; write to an anstream stream, so `--color` and
/// NO_COLOR are honoured) and return 1, or return a requested [`Exit`] code
/// quietly when one is anywhere in the chain.
pub fn report(
    err: &(dyn Error + 'static),
    log_pointer: Option<&str>,
    out: &mut impl Write,
) -> ExitCode {
    if let Some(code) = quiet_exit(err) {
        return ExitCode::from(code);
    }
    // Nothing better to do if stderr itself fails; the exit code still says so.
    let _ = out.write_all(error_block(err, log_pointer, theme::current()).as_bytes());
    ExitCode::from(1)
}

/// The error block for `err` in `theme`: `✗` and the message, each cause on
/// its own line, then the log pointer. Why not anyhow's `{:#}` or `{:?}`:
/// one line, or numbered causes and a backtrace, neither the spec's format.
pub fn error_block(
    err: &(dyn Error + 'static),
    log_pointer: Option<&str>,
    theme: &Theme,
) -> String {
    let mut lines = shown_chain(err).into_iter();
    let headline = lines.next().unwrap_or_default();
    block(&headline, lines, log_pointer, theme)
}

/// The error and its causes as they may be shown, outermost first: a
/// [`Sensitive`](crate::log::Sensitive) error and everything below it
/// become one [`WITHHELD`](crate::log::WITHHELD) line (term-ui decision 8).
/// Belt and braces: the marker's own Display is already the placeholder
/// and it has no cause; stopping at it here keeps that true if it changes.
pub fn shown_chain(err: &(dyn Error + 'static)) -> Vec<String> {
    let mut lines = Vec::new();
    for e in chain(err) {
        if e.downcast_ref::<crate::log::Sensitive>().is_some() {
            lines.push(crate::log::WITHHELD.to_string());
            break;
        }
        lines.push(e.to_string());
    }
    lines
}

fn block(
    headline: &str,
    causes: impl IntoIterator<Item = String>,
    log_pointer: Option<&str>,
    theme: &Theme,
) -> String {
    let causes = causes.into_iter().map(|cause| (Some("caused by:"), cause));
    let pointer = log_pointer.map(|pointer| (Some("details:"), pointer.to_string()));
    render_block(headline, causes.chain(pointer), theme)
}

/// sb's error block: the mark in the problem colour and the headline, then
/// indented lines, each with an optional muted label. A plain theme renders
/// no codes at all (anstyle renders an empty style as nothing).
fn render_block<'a>(
    headline: &str,
    lines: impl IntoIterator<Item = (Option<&'a str>, String)>,
    theme: &Theme,
) -> String {
    let (mark, label) = (theme.style(Role::Problem), theme.style(Role::Muted));
    let mut block = format!("{mark}✗{mark:#} {headline}\n");
    for (name, text) in lines {
        let _ = match name {
            Some(name) => writeln!(block, "  {label}{name}{label:#} {text}"),
            None => writeln!(block, "  {text}"),
        };
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancel::Shutdown;
    use std::fmt;

    /// An error with an optional cause, like a module's typed error.
    #[derive(Debug)]
    struct Failure {
        message: &'static str,
        cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    }

    impl fmt::Display for Failure {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.message)
        }
    }

    impl std::error::Error for Failure {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.cause
                .as_deref()
                .map(|c| c as &(dyn std::error::Error + 'static))
        }
    }

    fn failure(message: &'static str) -> Failure {
        Failure {
            message,
            cause: None,
        }
    }

    fn report_text(
        err: &(dyn std::error::Error + 'static),
        log: Option<&str>,
    ) -> (String, ExitCode) {
        let mut out = Vec::new();
        let code = report(err, log, &mut out);
        (String::from_utf8(out).expect("utf-8"), code)
    }

    #[test]
    fn an_error_is_one_block_and_exits_1() {
        let (text, code) = report_text(&failure("could not read the config"), None);
        assert_eq!(text, "✗ could not read the config\n");
        assert_eq!(code, ExitCode::from(1));
    }

    #[test]
    fn each_cause_gets_its_own_line() {
        let err = Failure {
            message: "could not read the config",
            cause: Some(Box::new(Failure {
                message: "permission denied",
                cause: Some(Box::new(failure("os error 13"))),
            })),
        };
        let (text, _) = report_text(&err, None);
        assert_eq!(
            text,
            "✗ could not read the config\n  caused by: permission denied\n  caused by: os error 13\n"
        );
    }

    #[test]
    fn the_log_pointer_comes_last() {
        let (text, _) = report_text(
            &failure("step failed"),
            Some("/var/log/sb/sb.2026-10-30.log (run 1a2b3c4d)"),
        );
        assert_eq!(
            text,
            "✗ step failed\n  details: /var/log/sb/sb.2026-10-30.log (run 1a2b3c4d)\n"
        );
    }

    /// A requested code (a child's own code, or "already shown") exits
    /// with that code and prints nothing more.
    #[test]
    fn a_requested_exit_is_quiet_and_keeps_its_code() {
        let (text, code) = report_text(&Exit(7), Some("/var/log/sb/x.log (run 1)"));
        assert_eq!(text, "");
        assert_eq!(code, ExitCode::from(7));
    }

    /// The reader went away (`sb version | head -0`): quiet, and the code a
    /// process killed by SIGPIPE gets (128 + 13), as sb-go's.
    #[test]
    fn a_broken_pipe_is_quiet_with_141() {
        let err = output_error(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        let (text, code) = report_text(&err, None);
        assert_eq!(text, "");
        assert_eq!(code, ExitCode::from(141));
    }

    /// A signal ended the run: "interrupted" (and the log pointer), never
    /// the command's own error chain, with the signal's code.
    #[test]
    fn an_interrupted_run_says_so_with_the_signal_code() {
        let shutdown = Shutdown::new();
        shutdown.signalled(signal_hook::consts::SIGINT);
        let mut out = Vec::new();
        let err = failure("operation cancelled");
        let code = finish_run(
            &shutdown,
            Some(&err),
            Some("/var/log/sb/x.log (run 1)"),
            &mut out,
        );
        assert_eq!(code, ExitCode::from(130));
        assert_eq!(
            String::from_utf8(out).expect("utf-8"),
            "✗ interrupted\n  details: /var/log/sb/x.log (run 1)\n"
        );
    }

    /// Asking to end with 0 ends successfully and quietly.
    #[test]
    fn a_requested_shutdown_with_0_succeeds() {
        let shutdown = Shutdown::new();
        shutdown.request(0);
        let mut out = Vec::new();
        let err = failure("cancelled");
        assert_eq!(
            finish_run(&shutdown, Some(&err), None, &mut out),
            ExitCode::from(0)
        );
        assert!(out.is_empty());
    }

    /// A command asked to end with its own code: quiet, with that code.
    #[test]
    fn a_requested_shutdown_is_quiet() {
        let shutdown = Shutdown::new();
        shutdown.request(3);
        let mut out = Vec::new();
        assert_eq!(
            finish_run(&shutdown, None, None, &mut out),
            ExitCode::from(3)
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_normal_run_ends_as_its_result_says() {
        let shutdown = Shutdown::new();
        let mut out = Vec::new();
        assert_eq!(
            finish_run(&shutdown, None, None, &mut out),
            ExitCode::SUCCESS
        );
        let err = failure("boom");
        assert_eq!(
            finish_run(&shutdown, Some(&err), None, &mut out),
            ExitCode::from(1)
        );
        assert_eq!(String::from_utf8(out).expect("utf-8"), "✗ boom\n");
    }

    /// One classification for the screen and the log: a requested code or
    /// a broken pipe is a quiet exit, anything else a failure.
    #[test]
    fn quiet_exits_are_classified_once() {
        assert_eq!(quiet_exit(&Exit(4)), Some(4));
        let pipe = output_error(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        assert_eq!(quiet_exit(&pipe), Some(141));
        assert_eq!(quiet_exit(&failure("boom")), None);
    }

    /// A broken pipe elsewhere (a child sb writes to died) is a real
    /// failure, not the reader of sb's output going away: sb-go reports it.
    #[test]
    fn a_broken_pipe_to_a_child_is_a_failure() {
        let err = Failure {
            message: "writing to ansible",
            cause: Some(Box::new(std::io::Error::from(
                std::io::ErrorKind::BrokenPipe,
            ))),
        };
        assert_eq!(quiet_exit(&err), None);
        let (text, code) = report_text(&err, None);
        assert_eq!(code, ExitCode::from(1));
        assert!(text.starts_with("✗ writing to ansible\n"), "{text}");
    }

    /// Other output errors (a full disk) stay errors.
    #[test]
    fn other_output_errors_stay_errors() {
        let err = output_error(std::io::Error::from(std::io::ErrorKind::StorageFull));
        assert_eq!(quiet_exit(&err), None);
    }

    /// SGR parameter lists in `text`, e.g. `[[38, 2, 255, 110, 122]]`. A
    /// plain split is enough: the input is anstyle's own rendering.
    fn sgr(text: &str) -> Vec<Vec<u16>> {
        text.split("\x1b[")
            .skip(1)
            .filter_map(|rest| rest.split_once('m'))
            .map(|(params, _)| params.split(';').filter_map(|p| p.parse().ok()).collect())
            .collect()
    }

    fn block_in(profile: crate::theme::Profile) -> String {
        let theme = crate::theme::Theme::resolve(&crate::theme::Palette::default(), profile);
        let err = Failure {
            message: "boom",
            cause: Some(Box::new(failure("why"))),
        };
        error_block(&err, Some("/var/log/sb/sb.2026-10-30.log (run 1)"), &theme)
    }

    /// Each profile's block uses only its kind of colour code (ADR-002's
    /// profile guards), and the text stays the same.
    #[test]
    fn each_profile_writes_only_its_kind_of_colour() {
        use crate::theme::Profile;
        let colours = |profile| -> Vec<Vec<u16>> {
            sgr(&block_in(profile))
                .into_iter()
                .filter(|c| !c.is_empty() && c.first() != Some(&0))
                .collect()
        };
        let truecolor = colours(Profile::TrueColor);
        assert!(
            !truecolor.is_empty() && truecolor.iter().all(|c| c.starts_with(&[38, 2])),
            "{truecolor:?}"
        );
        let ansi256 = colours(Profile::Ansi256);
        assert!(
            !ansi256.is_empty() && ansi256.iter().all(|c| c.starts_with(&[38, 5])),
            "{ansi256:?}"
        );
        let ansi = colours(Profile::Ansi);
        assert!(
            !ansi.is_empty()
                && ansi
                    .iter()
                    .all(|c| c.len() == 1 && matches!(c[0], 30..=37 | 90..=97)),
            "{ansi:?}"
        );
        assert!(!block_in(Profile::Ascii).contains('\x1b'));
        let plain = |text: &str| anstream::adapter::strip_str(text).to_string();
        assert_eq!(
            plain(&block_in(Profile::TrueColor)),
            "✗ boom\n  caused by: why\n  details: /var/log/sb/sb.2026-10-30.log (run 1)\n"
        );
        // Every profile writes the same text; only the colour codes differ.
        for profile in [Profile::Ansi256, Profile::Ansi, Profile::Ascii] {
            assert_eq!(
                plain(&block_in(profile)),
                plain(&block_in(Profile::TrueColor)),
                "{profile:?}"
            );
        }
    }

    /// The mark takes the problem colour.
    #[test]
    fn the_mark_is_in_the_problem_colour() {
        let block = block_in(crate::theme::Profile::TrueColor);
        assert!(block.starts_with("\x1b[38;2;255;110;122m✗"), "{block:?}");
    }

    /// A requested exit wrapped in context (as anyhow's `context` does)
    /// still wins.
    #[test]
    fn a_requested_exit_is_found_under_context() {
        let err = Failure {
            message: "running ansible",
            cause: Some(Box::new(Exit(4))),
        };
        let (text, code) = report_text(&err, None);
        assert_eq!(text, "");
        assert_eq!(code, ExitCode::from(4));
    }

    /// A sensitive error, and everything below it, shows as the
    /// placeholder; what is above it shows as usual (term-ui decision 8).
    #[test]
    fn a_sensitive_cause_is_withheld_on_screen() {
        let secret = std::io::Error::other("secret hunter2-not-real");
        let err = Failure {
            message: "Cloudflare check failed",
            cause: Some(Box::new(crate::log::Sensitive::new(secret))),
        };
        assert_eq!(
            shown_chain(&err),
            ["Cloudflare check failed", crate::log::WITHHELD]
        );
        let block = error_block(&err, None, &Theme::plain());
        assert!(!block.contains("hunter2"), "{block}");
        assert!(
            block.contains("caused by: (sensitive, withheld)"),
            "{block}"
        );
        let whole = crate::log::Sensitive::new(std::io::Error::other("hunter2"));
        assert_eq!(shown_chain(&whole), [crate::log::WITHHELD]);
    }

    /// A transparent wrapper (a common thiserror pattern) forwards Display
    /// and causes to what it wraps: the marker itself must say nothing, so
    /// the secret can't leak through it, nor through Debug.
    #[test]
    fn a_wrapped_sensitive_error_still_says_nothing() {
        #[derive(Debug, thiserror::Error)]
        enum Credentials {
            #[error(transparent)]
            Secret(#[from] crate::log::Sensitive),
        }
        let secret = std::io::Error::other("secret hunter2-not-real");
        let err = Credentials::from(crate::log::Sensitive::new(secret));
        assert_eq!(shown_chain(&err), [crate::log::WITHHELD]);
        let block = error_block(&err, None, &Theme::plain());
        assert!(!block.contains("hunter2"), "{block}");
        assert!(!format!("{err:?}").contains("hunter2"), "{err:?}");
        let marker = crate::log::Sensitive::new(std::io::Error::other("hunter2"));
        assert_eq!(format!("{marker:?}"), "Sensitive((sensitive, withheld))");
        assert!(!err.to_string().contains("hunter2"));
    }
}
