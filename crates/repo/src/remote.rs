//! Every fetch, clone and submodule update goes through one wrapper (repo
//! spec, "Remote operations"; sb-go's git/remote.go, 85440d7): git
//! streamed into the step with no timeout, retried once over HTTP/1.1 when
//! GitHub asks a public repository for credentials, and sb-go's
//! public-repository error when the retry is asked the same.

use sb_term_ui::{DisplayText, Labels, Step};

use crate::Repo;

/// How many of git's last lines the public-repository error carries (as
/// many as exec's failure blocks show).
const DETAIL_LINES: usize = 10;

/// GitHub asked a public repository for credentials twice, in sb-go's
/// words (git/remote.go:131-142).
#[derive(Debug, thiserror::Error)]
#[error(
    "GitHub unexpectedly requested authentication for the public {repo} repository.\n\n\
     The public {repo} repository does not require credentials. sb stopped instead of \
     prompting for a username. A retry using HTTP/1.1 also failed. This is likely a \
     temporary GitHub-specific problem; try again in a few minutes.\n\n\
     Git details:\n{details}"
)]
pub struct PublicRepoError {
    repo: String,
    details: String,
}

impl Repo {
    /// `git <args>`, a remote operation, streamed into `step` with no
    /// timeout (Ctrl-C and SIGTERM stop it through exec's group). If it
    /// fails and git's kept stderr shows GitHub asking for credentials
    /// ([`asks_credentials`]), it runs once more as `git -c
    /// http.version=HTTP/1.1 <args>` under a child step with sb-go's
    /// labels.
    ///
    /// # Errors
    ///
    /// exec's `Failed` or spawn error; [`PublicRepoError`] when the retry
    /// is asked for credentials too.
    pub(crate) async fn remote(
        &self,
        step: &mut Step,
        args: &[&'static str],
    ) -> anyhow::Result<()> {
        self.remote_with(step, args, &[]).await
    }

    /// [`Repo::remote`] with `values` (a URL, a path; shown in the log as
    /// given, as every value sb computes is) after the literal `args`.
    pub(crate) async fn remote_with(
        &self,
        step: &mut Step,
        args: &[&'static str],
        values: &[&str],
    ) -> anyhow::Result<()> {
        let command = |base: sb_exec::Cmd| {
            let base = args.iter().fold(base, |cmd, arg| cmd.arg(arg));
            values
                .iter()
                .fold(base, |cmd, value| cmd.shown_value(*value))
        };
        let streamed = command(self.git()).stream(step).await?;
        if !needs_retry(streamed.outcome.ending, &streamed.stderr_tail) {
            let _streamed = streamed.check()?;
            return Ok(());
        }
        let op = operation(args);
        let repo = &self.name;
        let labels = Labels::new(
            format!("Retrying {repo} Git {op} over HTTP/1.1"),
            format!("Retried {repo} Git {op} over HTTP/1.1"),
        )
        .failure(format!("{repo} Git {op} retry over HTTP/1.1"));
        step.step(labels, async |step| {
            let retried = command(self.git().arg("-c").arg("http.version=HTTP/1.1"))
                .stream(step)
                .await?;
            if needs_retry(retried.outcome.ending, &retried.stderr_tail) {
                return Err(PublicRepoError {
                    repo: repo.clone(),
                    details: details(&retried.stderr_tail),
                }
                .into());
            }
            let _retried = retried.check()?;
            Ok(())
        })
        .await?;
        Ok(())
    }
}

/// Whether a run that ended so, with this stderr, was GitHub asking for
/// credentials: git exited non-zero (not interrupted or killed, which sb
/// passes on as they are) and printed [`asks_credentials`]'s phrases.
fn needs_retry(ending: sb_exec::Ending, stderr: &str) -> bool {
    matches!(ending, sb_exec::Ending::Exited(code) if code != 0) && asks_credentials(stderr)
}

/// sb-go's `isGitHubAuthenticationFailure`: all three of git's phrases
/// when it may not prompt for a GitHub https remote's credentials. Case
/// counts, as in sb-go; git's children run with `LC_ALL=C`, so the
/// English text holds in any locale (decision 4).
fn asks_credentials(stderr: &str) -> bool {
    [
        "could not read ",
        "for 'https://github.com",
        "terminal prompts disabled",
    ]
    .iter()
    .all(|phrase| stderr.contains(phrase))
}

/// The operation named in the retry's labels: the first argument, or the
/// first two for `submodule update` (sb-go's `gitRemoteOperation`).
fn operation(args: &[&str]) -> String {
    match args {
        [] => "operation".to_string(),
        ["submodule", second, ..] => format!("submodule {second}"),
        [first, ..] => (*first).to_string(),
    }
}

/// git's last lines for the error, sanitized as term-ui draws untrusted
/// text (`DisplayText`, no colour). Why not exec's own tail: it is private
/// to `Captured::check`, and a stream keeps only `stderr_tail`.
fn details(stderr: &str) -> String {
    let text = DisplayText::from_bytes(stderr.trim_end().as_bytes(), false);
    let skip = text.lines().len().saturating_sub(DETAIL_LINES);
    text.lines().skip(skip).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASKED: &str = "Cloning into 'x'...\n\
        fatal: could not read Username for 'https://github.com': terminal prompts disabled\n";

    #[test]
    fn all_three_phrases_ask_for_credentials() {
        assert!(asks_credentials(ASKED));
    }

    #[test]
    fn each_phrase_missing_is_another_failure() {
        for missing in [
            "could not read ",
            "for 'https://github.com",
            "terminal prompts disabled",
        ] {
            let text = ASKED.replace(missing, "");
            assert!(!asks_credentials(&text), "{missing:?}");
        }
        assert!(!asks_credentials(""));
    }

    #[test]
    fn another_case_is_another_failure() {
        assert!(!asks_credentials(&ASKED.to_uppercase()));
        assert!(!asks_credentials(
            &ASKED.replace("github.com", "GitHub.com")
        ));
    }

    #[test]
    fn only_a_failed_exit_with_the_phrases_is_retried() {
        use sb_exec::Ending;
        assert!(needs_retry(Ending::Exited(128), ASKED));
        assert!(needs_retry(Ending::Exited(1), ASKED));
        for ending in [
            Ending::Exited(0),
            Ending::Interrupted,
            Ending::TimedOut,
            Ending::Signalled(15),
        ] {
            assert!(!needs_retry(ending, ASKED), "{ending:?}");
        }
        assert!(!needs_retry(Ending::Exited(128), "fatal: other"));
    }

    #[test]
    fn the_operation_is_sb_gos() {
        assert_eq!(operation(&["fetch", "--progress"]), "fetch");
        assert_eq!(operation(&["clone", "--depth", "1"]), "clone");
        assert_eq!(
            operation(&["submodule", "update", "--init"]),
            "submodule update"
        );
        assert_eq!(operation(&["submodule"]), "submodule");
        assert_eq!(operation(&[]), "operation");
    }

    #[test]
    fn details_are_the_last_lines_sanitized() {
        let many = (1..=12)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            details(&many),
            (3..=12)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert_eq!(details("a\x1b]0;title\x07b\x1b[31mc\n"), "abc");
    }

    #[test]
    fn the_public_repo_error_is_sb_gos_text() {
        let error = PublicRepoError {
            repo: "Saltbox".into(),
            details: "fatal: x".into(),
        };
        assert_eq!(
            error.to_string(),
            "GitHub unexpectedly requested authentication for the public Saltbox repository.\n\n\
             The public Saltbox repository does not require credentials. sb stopped instead of \
             prompting for a username. A retry using HTTP/1.1 also failed. This is likely a \
             temporary GitHub-specific problem; try again in a few minutes.\n\n\
             Git details:\nfatal: x"
        );
    }
}
