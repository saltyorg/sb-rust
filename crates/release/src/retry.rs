//! The retry policy for metadata and downloads (release spec, "Download"
//! and decision 7), as sb-go's release/retry.go: up to [`ATTEMPTS`]
//! attempts on a network error, 403, 429, 5xx or a short body; waits of
//! 1, 2 and 4 s, or what `Retry-After` asks (seconds or an HTTP date), or
//! until `X-RateLimit-Reset` when `X-RateLimit-Remaining` is `0`; a wait
//! over [`MAX_WAIT`] gives up with the reason.
//!
//! Why not reqwest's `retry` module: it classifies and budgets only, with
//! no `Retry-After`, no backoff and no retry of a short body
//! (<https://docs.rs/reqwest/0.13.5/reqwest/retry/index.html>). Its
//! default stays on underneath: a request the server refused unprocessed
//! (an HTTP/2 `GOAWAY` or `REFUSED_STREAM`) is sent again within one attempt,
//! which is safe for these GETs. Why not tower's `retry`: its
//! `ExponentialBackoff` always adds jitter (sb-go's waits are exact), and
//! a `Policy` sees the call, not the body, so a short body is out of its
//! reach (<https://docs.rs/tower/0.5.3/tower/retry/index.html>). The loop
//! is ours; its waits are tokio's `sleep`, cancelled with the step.

use std::time::Duration;

use jiff::Timestamp;
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, RETRY_AFTER};

/// Attempts per request, the first included.
pub(crate) const ATTEMPTS: u32 = 4;
/// The first wait; each later one doubles it.
const BASE: Duration = Duration::from_secs(1);
/// A longer wait gives up instead.
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(60);

/// A status worth another attempt: 403 (GitHub's rate limit), 429, 5xx.
pub(crate) fn retryable(status: StatusCode) -> bool {
    status == StatusCode::FORBIDDEN
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

/// The wait after failed attempt `attempt` (from 1): the backoff, or what
/// the failed response's `headers` ask, measured from `now`. `Err` with
/// the wait when it is over [`MAX_WAIT`]. A header that can't be read, or
/// a time already past, leaves the backoff (sb-go's `retryDelay`).
pub(crate) fn wait(
    attempt: u32,
    headers: Option<&HeaderMap>,
    now: Timestamp,
) -> Result<Duration, Duration> {
    // The cap only keeps the shift in range; any attempt past 7 is over
    // the limit anyway.
    let doubling = 1u32 << attempt.saturating_sub(1).min(16);
    let mut wait = BASE.saturating_mul(doubling);
    if let Some(headers) = headers {
        let text = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
        };
        let retry_after = text(RETRY_AFTER.as_str()).unwrap_or_default();
        if let Ok(seconds) = retry_after.parse::<u64>() {
            wait = Duration::from_secs(seconds);
        } else if let Some(at) = http_date(retry_after) {
            wait = until(now, at).unwrap_or(wait);
        } else if text("x-ratelimit-remaining") == Some("0")
            && let Some(reset) = text("x-ratelimit-reset")
                .and_then(|reset| reset.parse::<i64>().ok())
                .and_then(|reset| Timestamp::from_second(reset).ok())
        {
            wait = until(now, reset).unwrap_or(wait);
        }
    }
    if wait > MAX_WAIT { Err(wait) } else { Ok(wait) }
}

/// An HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`), read by jiff's RFC 2822
/// parser, of which RFC 9110's preferred form is a subset
/// (<https://docs.rs/jiff/0.2.38/jiff/fmt/rfc2822/struct.DateTimeParser.html#method.parse_timestamp>).
/// RFC 9110's two obsolete forms aren't read: they leave the backoff.
fn http_date(text: &str) -> Option<Timestamp> {
    jiff::fmt::rfc2822::DateTimeParser::new()
        .parse_timestamp(text)
        .ok()
}

/// From `now` to `at`, when `at` is later.
fn until(now: Timestamp, at: Timestamp) -> Option<Duration> {
    Duration::try_from(at.duration_since(now))
        .ok()
        .filter(|wait| !wait.is_zero())
}

/// Why a retry was given up: the wait it would have needed, in whole
/// seconds rounded up, against the limit.
pub(crate) fn too_long(wait: Duration) -> String {
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    format!(
        "retry wait {seconds}s exceeds the {}s limit",
        MAX_WAIT.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, value.parse().expect("value"));
        }
        map
    }

    fn now() -> Timestamp {
        Timestamp::from_second(1_700_000_000).expect("now")
    }

    #[test]
    fn the_backoff_doubles_from_one_second() {
        let waits: Vec<_> = (1..=3).map(|attempt| wait(attempt, None, now())).collect();
        assert_eq!(
            waits,
            [1, 2, 4].map(|s| Ok(Duration::from_secs(s))).to_vec()
        );
        // Without the headers it asks about, a response keeps the backoff.
        assert_eq!(
            wait(2, Some(&headers(&[("x-other", "1")])), now()),
            Ok(Duration::from_secs(2))
        );
        assert_eq!(wait(u32::MAX, None, now()).map_err(|_| ()), Err(()));
    }

    #[test]
    fn retry_after_seconds_are_waited() {
        let asked = headers(&[("retry-after", " 7 ")]);
        assert_eq!(wait(1, Some(&asked), now()), Ok(Duration::from_secs(7)));
        let zero = headers(&[("retry-after", "0")]);
        assert_eq!(wait(3, Some(&zero), now()), Ok(Duration::ZERO));
        let negative = headers(&[("retry-after", "-5")]);
        assert_eq!(wait(1, Some(&negative), now()), Ok(Duration::from_secs(1)));
    }

    #[test]
    fn a_retry_after_date_is_waited_for() {
        let at = Timestamp::from_second(1_700_000_030).expect("at");
        let date = jiff::fmt::rfc2822::DateTimePrinter::new()
            .timestamp_to_rfc9110_string(&at)
            .expect("date");
        let asked = headers(&[("retry-after", &date)]);
        assert_eq!(wait(1, Some(&asked), now()), Ok(Duration::from_secs(30)));
        // A date already past keeps the backoff.
        let past = headers(&[("retry-after", "Thu, 01 Jan 1970 00:00:01 GMT")]);
        assert_eq!(wait(2, Some(&past), now()), Ok(Duration::from_secs(2)));
        let garbage = headers(&[("retry-after", "soon")]);
        assert_eq!(wait(1, Some(&garbage), now()), Ok(Duration::from_secs(1)));
    }

    #[test]
    fn a_spent_rate_limit_waits_for_its_reset() {
        let spent = headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1700000020"),
        ]);
        assert_eq!(wait(1, Some(&spent), now()), Ok(Duration::from_secs(20)));
        // Requests left: the reset is no reason to wait.
        let left = headers(&[
            ("x-ratelimit-remaining", "5"),
            ("x-ratelimit-reset", "1700000020"),
        ]);
        assert_eq!(wait(1, Some(&left), now()), Ok(Duration::from_secs(1)));
        // Retry-After comes first.
        let both = headers(&[
            ("retry-after", "3"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1700000020"),
        ]);
        assert_eq!(wait(1, Some(&both), now()), Ok(Duration::from_secs(3)));
    }

    #[test]
    fn a_wait_over_a_minute_gives_up() {
        let at_limit = headers(&[("retry-after", "60")]);
        assert_eq!(wait(1, Some(&at_limit), now()), Ok(MAX_WAIT));
        let over = headers(&[("retry-after", "61")]);
        assert_eq!(wait(1, Some(&over), now()), Err(Duration::from_secs(61)));
        let two_hours = headers(&[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1700007200"),
        ]);
        let err = wait(1, Some(&two_hours), now()).expect_err("too long");
        assert_eq!(too_long(err), "retry wait 7200s exceeds the 60s limit");
        assert_eq!(
            too_long(Duration::from_millis(60_001)),
            "retry wait 61s exceeds the 60s limit"
        );
    }

    #[test]
    fn retryable_statuses_are_sb_gos() {
        for code in [403, 429, 500, 502, 503, 599] {
            assert!(
                retryable(StatusCode::from_u16(code).expect("code")),
                "{code}"
            );
        }
        for code in [200, 301, 400, 401, 404, 410, 499] {
            assert!(
                !retryable(StatusCode::from_u16(code).expect("code")),
                "{code}"
            );
        }
    }
}
