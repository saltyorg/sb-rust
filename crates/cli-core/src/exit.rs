//! Exit codes and the error block (spec: "Exit codes", "Errors").
//!
//! Commands return any `std::error::Error`; the binary uses anyhow, whose
//! errors expose their causes through `source()` like any other. Parse
//! errors never reach here: clap prints them and exits 2 itself.

use std::error::Error;
use std::fmt::Write as _;
use std::io::Write;
pub use std::process::ExitCode;

/// End the run with this exit code and print nothing more: a wrapped
/// child's own code, or an error the command has already shown.
#[derive(Debug, thiserror::Error)]
#[error("exit code {0}")]
pub struct Exit(pub u8);

/// Print one error block (`✗` and the message, each cause on its own line,
/// then the log pointer) and return 1, or return a requested [`Exit`] code
/// quietly when one is anywhere in the chain.
pub fn report(
    err: &(dyn Error + 'static),
    log_pointer: Option<&str>,
    out: &mut impl Write,
) -> ExitCode {
    // Why not `Error::sources()`: still unstable (`error_iter`).
    let chain = || std::iter::successors(Some(err), |&e| e.source());
    if let Some(Exit(code)) = chain().find_map(|e| e.downcast_ref::<Exit>()) {
        return ExitCode::from(*code);
    }
    // The reader of our output went away (`sb ... | head`). Rust ignores
    // SIGPIPE, so this arrives as an error; end quietly with the code a
    // process killed by SIGPIPE gets (128 + 13), as sb-go does. Why not
    // `-Zon-broken-pipe=kill`: unstable, so it would need an ADR
    // (https://doc.rust-lang.org/beta/unstable-book/compiler-flags/on-broken-pipe.html);
    // why not resetting SIGPIPE with libc: unsafe code, forbidden here.
    let broken_pipe = chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(|e| e.kind() == std::io::ErrorKind::BrokenPipe);
    if broken_pipe {
        return ExitCode::from(141);
    }
    // The spec's block; why not anyhow's `{:#}` or `{:?}`: one line, or
    // numbered causes and a backtrace, neither the spec's format.
    let mut block = format!("✗ {err}\n");
    for cause in chain().skip(1) {
        let _ = writeln!(block, "  caused by: {cause}");
    }
    if let Some(pointer) = log_pointer {
        let _ = writeln!(block, "  details: {pointer}");
    }
    // Nothing better to do if stderr itself fails; the exit code still says so.
    let _ = out.write_all(block.as_bytes());
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let err = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        let (text, code) = report_text(&err, None);
        assert_eq!(text, "");
        assert_eq!(code, ExitCode::from(141));
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
}
