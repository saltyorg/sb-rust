//! The parallel runner (motd spec, Command flow step 6; decisions 11 and
//! 26): each section is one tokio task, capped at sb-go's 30 s; a section
//! that panics or runs over shows its error line and the others still
//! show; results come back in display order, whatever order they end in.
//!
//! Library-backed: `tokio::spawn` catches a task's panic and hands it back
//! through `JoinError::try_into_panic`
//! (<https://docs.rs/tokio/1.54.1/tokio/task/struct.JoinError.html#method.try_into_panic>),
//! and `tokio::time::timeout_at` caps the wait
//! (<https://docs.rs/tokio/1.54.1/tokio/time/fn.timeout_at.html>). Every
//! section starts at once, so one deadline for all is each one's 30 s.
//! At that deadline the run's token is cancelled, which reaches each
//! section that takes it (Services' systemctl calls), and the task is
//! aborted; a child still running is ended with its dropped run (sb-exec
//! kills the group).

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::redact::Secrets;
use crate::{Out, Section};

/// sb-go's `defaultProviderTimeout` (`motd/parallel.go`), per section.
pub const CAP: Duration = Duration::from_secs(30);

/// One section's collection: its line (empty to hide it) and its
/// `--json` form.
pub type Job = Pin<Box<dyn Future<Output = Out> + Send + 'static>>;

/// Each job run as its own task under `cap`, its value or error line
/// returned in display order (the jobs' `Section` order). `token` is
/// cancelled when the cap passes with a job still running; a panic's
/// message has `secrets` hidden.
pub async fn collect(
    jobs: Vec<(Section, Job)>,
    cap: Duration,
    token: &CancellationToken,
    secrets: &Secrets,
) -> Vec<(Section, Out)> {
    let start = Instant::now();
    let deadline = start + cap;
    let mut tasks: Vec<_> = jobs
        .into_iter()
        .map(|(section, job)| {
            let handle = tokio::spawn(async move {
                let value = job.await;
                (value, start.elapsed())
            });
            (section, handle)
        })
        .collect();
    tasks.sort_by_key(|(section, _)| *section);
    let mut results = Vec::with_capacity(tasks.len());
    for (section, mut handle) in tasks {
        let value = match tokio::time::timeout_at(deadline, &mut handle).await {
            Ok(Ok((value, elapsed))) => {
                let outcome = if value.text.is_empty() {
                    "empty"
                } else {
                    "shown"
                };
                log(section, outcome, elapsed);
                value
            }
            Ok(Err(err)) => {
                log(section, "panicked", start.elapsed());
                Out::error(panicked(err.try_into_panic().ok().as_deref(), secrets))
            }
            Err(_) => {
                token.cancel();
                // Dropping a JoinHandle leaves its task running: abort it.
                handle.abort();
                log(section, "timed out", cap);
                Out::error(timed_out(cap))
            }
        };
        results.push((section, value));
    }
    results
}

/// The run log's record for one section (criterion 12).
fn log(section: Section, outcome: &str, elapsed: Duration) {
    let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        section = section.flag(),
        outcome,
        elapsed_ms,
        "motd section"
    );
}

/// sb-go's line for a section over its cap, the duration as Go prints a
/// whole number of seconds (`30s`).
fn timed_out(cap: Duration) -> String {
    format!("Error: provider timed out after {}s", cap.as_secs())
}

/// sb-go's line for a panic (`motd/parallel.go`): the message made safe to
/// draw and cut to its first line, motd.yml's credentials hidden before
/// and after (decision 12). Why not `JoinError`'s
/// `Display`: it reads `task <id> panicked with message "<msg>"`, and
/// tokio's payload-to-text helper is private
/// (tokio 1.54.1 `src/runtime/task/error.rs`, `panic_payload_as_str`).
fn panicked(payload: Option<&(dyn Any + Send)>, secrets: &Secrets) -> String {
    let message = panic_message(payload);
    let shown = secrets.hide(&crate::text::one_line(secrets.hide(message).as_bytes()));
    format!("Error: panic occurred ({shown})")
}

/// A panic's message: its `&str` or `String` payload, or what tokio
/// writes for any other (`Box<dyn Any>`).
pub(crate) fn panic_message(payload: Option<&(dyn Any + Send)>) -> &str {
    payload
        .and_then(|p| {
            p.downcast_ref::<&str>()
                .copied()
                .or_else(|| p.downcast_ref::<String>().map(String::as_str))
        })
        .unwrap_or("Box<dyn Any>")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each section's line, as the screen gets it.
    fn texts(results: Vec<(Section, Out)>) -> Vec<(Section, String)> {
        results
            .into_iter()
            .map(|(section, out)| (section, out.text))
            .collect()
    }

    fn job(value: &'static str, after: Duration) -> Job {
        Box::pin(async move {
            tokio::time::sleep(after).await;
            Out::from(value.to_string())
        })
    }

    #[tokio::test(start_paused = true)]
    async fn results_come_in_display_order_whatever_order_they_end_in() {
        let jobs = vec![
            (Section::Kernel, job("k", Duration::from_secs(1))),
            (Section::Distribution, job("d", Duration::from_secs(2))),
            (Section::Uptime, job("", Duration::ZERO)),
        ];
        let results =
            texts(collect(jobs, CAP, &CancellationToken::new(), &Secrets::default()).await);
        assert_eq!(
            results,
            [
                (Section::Distribution, "d".to_string()),
                (Section::Kernel, "k".to_string()),
                (Section::Uptime, String::new()),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_section_past_the_cap_shows_sb_gos_line_and_the_rest_still_show() {
        let start = Instant::now();
        let jobs = vec![
            (Section::Distribution, job("d", Duration::from_secs(29))),
            (Section::Kernel, job("never", Duration::from_secs(3600))),
            (Section::Uptime, job("u", Duration::from_secs(1))),
        ];
        let results =
            texts(collect(jobs, CAP, &CancellationToken::new(), &Secrets::default()).await);
        assert_eq!(
            results,
            [
                (Section::Distribution, "d".to_string()),
                (
                    Section::Kernel,
                    "Error: provider timed out after 30s".to_string()
                ),
                (Section::Uptime, "u".to_string()),
            ]
        );
        // The whole run took the cap, not the slow section's hour.
        assert_eq!(start.elapsed(), CAP);
    }

    /// The cap reaches a section through the run's token: cancelled when
    /// a job runs past it, so the job's own calls see it (Services), and
    /// left alone when every job ends in time.
    #[tokio::test(start_paused = true)]
    async fn the_cap_cancels_the_runs_token() {
        let token = CancellationToken::new();
        let seen = token.clone();
        let watcher: Job = Box::pin(async move {
            seen.cancelled().await;
            Out::from("never shown".to_string())
        });
        let jobs = vec![
            (Section::Services, watcher),
            (Section::Kernel, job("k", Duration::from_secs(1))),
        ];
        let results = texts(collect(jobs, CAP, &token, &Secrets::default()).await);
        assert!(token.is_cancelled());
        assert_eq!(results[0].1, "k");
        assert_eq!(results[1].1, "Error: provider timed out after 30s");
        let token = CancellationToken::new();
        collect(
            vec![(Section::Kernel, job("k", Duration::from_secs(29)))],
            CAP,
            &token,
            &Secrets::default(),
        )
        .await;
        assert!(!token.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn two_slow_sections_share_one_deadline() {
        let start = Instant::now();
        let jobs = vec![
            (Section::Distribution, job("x", Duration::from_secs(3600))),
            (Section::Kernel, job("y", Duration::from_secs(3600))),
        ];
        let results =
            texts(collect(jobs, CAP, &CancellationToken::new(), &Secrets::default()).await);
        assert!(results.iter().all(|(_, v)| v.contains("timed out")));
        assert_eq!(start.elapsed(), CAP);
    }

    #[tokio::test]
    async fn a_panic_shows_its_message_and_the_rest_still_show() {
        let jobs: Vec<(Section, Job)> = vec![
            (
                Section::Distribution,
                Box::pin(async { panic!("bad \x1b]0;title\x07thing\nsecond line") }),
            ),
            (
                Section::Kernel,
                Box::pin(async { std::panic::panic_any(format!("owned {}", 7)) }),
            ),
            (
                Section::Uptime,
                Box::pin(async { Out::from("u".to_string()) }),
            ),
            (
                Section::Processes,
                Box::pin(async { std::panic::panic_any(7_u8) }),
            ),
        ];
        let results =
            texts(collect(jobs, CAP, &CancellationToken::new(), &Secrets::default()).await);
        assert_eq!(
            results,
            [
                (
                    Section::Distribution,
                    "Error: panic occurred (bad thing)".to_string()
                ),
                (
                    Section::Kernel,
                    "Error: panic occurred (owned 7)".to_string()
                ),
                (Section::Uptime, "u".to_string()),
                (
                    Section::Processes,
                    "Error: panic occurred (Box<dyn Any>)".to_string()
                ),
            ]
        );
    }

    /// A panic's message never shows a credential, even one split by a
    /// control character the message's drawing drops.
    #[tokio::test]
    async fn a_panic_hides_the_credentials() {
        let jobs: Vec<(Section, Job)> = vec![(
            Section::Plex,
            Box::pin(async { panic!("token run-canary-42 and run-\x07canary-42") }),
        )];
        let secrets = Secrets::from_values(["run-canary-42"]);
        let results = texts(collect(jobs, CAP, &CancellationToken::new(), &secrets).await);
        assert_eq!(
            results[0].1,
            "Error: panic occurred (token [REDACTED] and [REDACTED])"
        );
    }

    #[test]
    fn the_cap_is_sb_gos_thirty_seconds() {
        assert_eq!(CAP, Duration::from_secs(30));
        assert_eq!(timed_out(CAP), "Error: provider timed out after 30s");
    }
}
