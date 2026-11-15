//! A child process that failed, as a step's error (exec spec, decision 5;
//! term-ui's endings table): exec's `Failed` is this type, so term-ui can
//! show how the child ended in the times column (`exit 3 · 2.0s`) without
//! depending on exec.

/// A failed child: its message (the command, arguments masked as in the
/// log, and how it ended), the short reason for the times column (`exit
/// 3`, `SIGTERM`, `timed out`) and its stderr's last lines (from
/// `capture`; empty when the step's own lines show its output).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ChildFailure {
    message: String,
    reason: String,
    last_lines: Vec<String>,
}

impl ChildFailure {
    /// A failure reading `message`, with `reason` in the times column and
    /// `last_lines` shown under the message.
    pub fn new(
        message: impl Into<String>,
        reason: impl Into<String>,
        last_lines: Vec<String>,
    ) -> Self {
        Self {
            message: message.into(),
            reason: reason.into(),
            last_lines,
        }
    }

    /// How the child ended, short: the times column's reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// The child's last lines shown with the failure, secrets taken out by
    /// whoever made it.
    #[must_use]
    pub fn last_lines(&self) -> &[String] {
        &self.last_lines
    }
}
