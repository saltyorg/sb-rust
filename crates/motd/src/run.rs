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

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::time::Instant;

use crate::Section;

/// sb-go's `defaultProviderTimeout` (`motd/parallel.go`), per section.
pub const CAP: Duration = Duration::from_secs(30);

/// One section's collection: its value, empty to hide the line.
pub type Job = Pin<Box<dyn Future<Output = String> + Send + 'static>>;

/// Each job run as its own task under `cap`, its value or error line
/// returned in display order (the jobs' `Section` order).
pub async fn collect(jobs: Vec<(Section, Job)>, cap: Duration) -> Vec<(Section, String)> {
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
                let outcome = if value.is_empty() { "empty" } else { "shown" };
                log(section, outcome, elapsed);
                value
            }
            Ok(Err(err)) => {
                log(section, "panicked", start.elapsed());
                panicked(err.try_into_panic().ok().as_deref())
            }
            Err(_) => {
                // Dropping a JoinHandle leaves its task running: abort it.
                handle.abort();
                log(section, "timed out", cap);
                timed_out(cap)
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
/// draw and cut to its first line. Credentials are hidden here too once
/// motd.yml brings them (decision 12, M44). Why not `JoinError`'s
/// `Display`: it reads `task <id> panicked with message "<msg>"`, and
/// tokio's payload-to-text helper is private
/// (tokio 1.54.1 `src/runtime/task/error.rs`, `panic_payload_as_str`).
fn panicked(payload: Option<&(dyn Any + Send)>) -> String {
    let message = payload
        .and_then(|p| {
            p.downcast_ref::<&str>()
                .copied()
                .or_else(|| p.downcast_ref::<String>().map(String::as_str))
        })
        .unwrap_or("Box<dyn Any>");
    let shown = crate::text::one_line(message.as_bytes());
    format!("Error: panic occurred ({shown})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(value: &'static str, after: Duration) -> Job {
        Box::pin(async move {
            tokio::time::sleep(after).await;
            value.to_string()
        })
    }

    #[tokio::test(start_paused = true)]
    async fn results_come_in_display_order_whatever_order_they_end_in() {
        let jobs = vec![
            (Section::Kernel, job("k", Duration::from_secs(1))),
            (Section::Distribution, job("d", Duration::from_secs(2))),
            (Section::Uptime, job("", Duration::ZERO)),
        ];
        let results = collect(jobs, CAP).await;
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
        let results = collect(jobs, CAP).await;
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

    #[tokio::test(start_paused = true)]
    async fn two_slow_sections_share_one_deadline() {
        let start = Instant::now();
        let jobs = vec![
            (Section::Distribution, job("x", Duration::from_secs(3600))),
            (Section::Kernel, job("y", Duration::from_secs(3600))),
        ];
        let results = collect(jobs, CAP).await;
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
            (Section::Uptime, Box::pin(async { "u".to_string() })),
            (
                Section::Processes,
                Box::pin(async { std::panic::panic_any(7_u8) }),
            ),
        ];
        let results = collect(jobs, CAP).await;
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

    #[test]
    fn the_cap_is_sb_gos_thirty_seconds() {
        assert_eq!(CAP, Duration::from_secs(30));
        assert_eq!(timed_out(CAP), "Error: provider timed out after 30s");
    }
}
